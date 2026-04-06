//! Integration tests encoding every validation boundary from the firmware's
//! `radio_validate_channel_params` function at `0xC003C694`.
//!
//! These tests verify the exact accept/reject boundaries for each validated
//! type, ensuring 1:1 correspondence with firmware reverse-engineering data.

use kenwood_thd75::error::ValidationError;
use kenwood_thd75::types::tone::{CTCSS_FREQUENCIES, DCS_CODES};
use kenwood_thd75::types::*;

// ============================================================================
// 1. Boundary tests — exact boundary for each validated type (12 tests)
// ============================================================================

#[test]
fn tone_code_boundary() {
    assert!(ToneCode::new(49).is_ok(), "49 is the last CTCSS tone code");
    assert!(
        ToneCode::new(50).is_ok(),
        "50 is the 1750 Hz tone burst (ARFC RE)"
    );
    assert!(ToneCode::new(51).is_err(), "51 must be rejected");
}

#[test]
fn band_boundary() {
    assert!(Band::try_from(13u8).is_ok(), "13 is the last valid band");
    assert!(Band::try_from(14u8).is_err(), "14 must be rejected");
}

#[test]
fn mode_boundary() {
    assert!(Mode::try_from(7u8).is_ok(), "7 (DR) is valid");
    assert!(Mode::try_from(8u8).is_ok(), "8 (WFM) confirmed by ARFC RE");
    assert!(Mode::try_from(9u8).is_ok(), "9 (CW-R) confirmed by ARFC RE");
    assert!(Mode::try_from(10u8).is_err(), "10 must be rejected");
}

#[test]
fn power_level_boundary() {
    assert!(
        PowerLevel::try_from(3u8).is_ok(),
        "3 (ExtraLow) is the last valid power level"
    );
    assert!(PowerLevel::try_from(4u8).is_err(), "4 must be rejected");
}

#[test]
fn tone_mode_boundary() {
    assert!(ToneMode::try_from(2u8).is_ok(), "2 (DCS) is valid");
    assert!(
        ToneMode::try_from(3u8).is_ok(),
        "3 (CrossTone) confirmed by ARFC RE"
    );
    assert!(ToneMode::try_from(4u8).is_err(), "4 must be rejected");
}

#[test]
fn shift_direction_boundary() {
    assert!(
        ShiftDirection::try_from(15u8).is_ok(),
        "15 is the last valid shift direction (4-bit field)"
    );
    assert!(
        ShiftDirection::try_from(16u8).is_err(),
        "16 must be rejected"
    );
}

#[test]
fn step_size_boundary() {
    assert!(
        StepSize::try_from(11u8).is_ok(),
        "11 (100kHz) is the last valid step size"
    );
    assert!(StepSize::try_from(12u8).is_err(), "12 must be rejected");
}

#[test]
fn data_speed_boundary() {
    assert!(
        DataSpeed::try_from(1u8).is_ok(),
        "1 (9600bps) is the last valid data speed"
    );
    assert!(DataSpeed::try_from(2u8).is_err(), "2 must be rejected");
}

#[test]
fn lockout_boundary() {
    assert!(
        LockoutMode::try_from(2u8).is_ok(),
        "2 (Group) is the last valid lockout mode"
    );
    assert!(LockoutMode::try_from(3u8).is_err(), "3 must be rejected");
}

#[test]
fn ctcss_mode_boundary() {
    assert!(
        CtcssMode::try_from(2u8).is_ok(),
        "2 (EncodeOnly) is the last valid CTCSS mode"
    );
    assert!(CtcssMode::try_from(3u8).is_err(), "3 must be rejected");
}

#[test]
fn channel_name_length_boundary() {
    assert!(
        ChannelName::new("12345678").is_ok(),
        "8-char name must be accepted"
    );
    assert!(
        ChannelName::new("123456789").is_err(),
        "9-char name must be rejected"
    );
}

#[test]
fn dcs_code_index_boundary() {
    assert!(DcsCode::new(103).is_ok(), "103 is the last valid DCS index");
    assert!(DcsCode::new(104).is_err(), "104 must be rejected");
}

// ============================================================================
// 2. Full-range exhaustive tests (4 tests)
// ============================================================================

#[test]
fn tone_code_full_valid_range() {
    for i in 0u8..=50 {
        assert!(ToneCode::new(i).is_ok(), "ToneCode({i}) should be valid");
    }
    for i in 51u8..=255 {
        assert!(ToneCode::new(i).is_err(), "ToneCode({i}) should be invalid");
    }
}

#[test]
fn band_full_valid_range() {
    for i in 0u8..14 {
        assert!(Band::try_from(i).is_ok(), "Band({i}) should be valid");
    }
    for i in 14u8..=255 {
        assert!(Band::try_from(i).is_err(), "Band({i}) should be invalid");
    }
}

#[test]
fn mode_full_valid_range() {
    for i in 0u8..=9 {
        assert!(Mode::try_from(i).is_ok(), "Mode({i}) should be valid");
    }
    for i in 10u8..=255 {
        assert!(Mode::try_from(i).is_err(), "Mode({i}) should be invalid");
    }
}

#[test]
fn dcs_code_full_valid_range() {
    for i in 0u8..104 {
        assert!(DcsCode::new(i).is_ok(), "DcsCode({i}) should be valid");
    }
    for i in 104u8..=255 {
        assert!(DcsCode::new(i).is_err(), "DcsCode({i}) should be invalid");
    }
}

// ============================================================================
// 3. Table content tests (5 tests)
// ============================================================================

#[test]
fn ctcss_frequency_table_every_entry() {
    let expected: [f64; 51] = [
        67.0, 69.3, 71.9, 74.4, 77.0, 79.7, 82.5, 85.4, 88.5, 91.5, 94.8, 97.4, 100.0, 103.5,
        107.2, 110.9, 114.8, 118.8, 123.0, 127.3, 131.8, 136.5, 141.3, 146.2, 151.4, 156.7, 159.8,
        162.2, 165.5, 167.9, 171.3, 173.8, 177.3, 179.9, 183.5, 186.2, 189.9, 192.8, 196.6, 199.5,
        203.5, 206.5, 210.7, 218.1, 225.7, 229.1, 233.6, 241.8, 250.3, 254.1,
        1750.0, // Code 50: 1750 Hz tone burst (ARFC RE, not a CTCSS tone)
    ];
    assert_eq!(CTCSS_FREQUENCIES.len(), expected.len());
    for (i, (&actual, &exp)) in CTCSS_FREQUENCIES.iter().zip(expected.iter()).enumerate() {
        assert!(
            (actual - exp).abs() < f64::EPSILON,
            "CTCSS_FREQUENCIES[{i}]: expected {exp}, got {actual}"
        );
    }
}

#[test]
fn ctcss_table_monotonically_increasing() {
    for i in 1..CTCSS_FREQUENCIES.len() {
        assert!(
            CTCSS_FREQUENCIES[i] > CTCSS_FREQUENCIES[i - 1],
            "CTCSS_FREQUENCIES[{i}] ({}) must be > [{prev}] ({})",
            CTCSS_FREQUENCIES[i],
            CTCSS_FREQUENCIES[i - 1],
            prev = i - 1,
        );
    }
}

#[test]
fn dcs_code_table_every_entry() {
    let expected: [u16; 104] = [
        23, 25, 26, 31, 32, 36, 43, 47, 51, 53, 54, 65, 71, 72, 73, 74, 114, 115, 116, 122, 125,
        131, 132, 134, 143, 145, 152, 155, 156, 162, 165, 172, 174, 205, 212, 223, 225, 226, 243,
        244, 245, 246, 251, 252, 255, 261, 263, 265, 266, 271, 274, 306, 311, 315, 325, 331, 332,
        343, 346, 351, 356, 364, 365, 371, 411, 412, 413, 423, 431, 432, 445, 446, 452, 454, 455,
        462, 464, 465, 466, 503, 506, 516, 523, 526, 532, 546, 565, 606, 612, 624, 627, 631, 632,
        654, 662, 664, 703, 712, 723, 731, 732, 734, 743, 754,
    ];
    assert_eq!(DCS_CODES.len(), expected.len());
    for (i, (&actual, &exp)) in DCS_CODES.iter().zip(expected.iter()).enumerate() {
        assert_eq!(actual, exp, "DCS_CODES[{i}]: expected {exp}, got {actual}");
    }
}

#[test]
fn dcs_code_table_no_zero() {
    for (i, &code) in DCS_CODES.iter().enumerate() {
        assert_ne!(code, 0, "DCS_CODES[{i}] must not be zero");
    }
}

#[test]
fn dcs_code_table_no_duplicates() {
    let mut sorted = DCS_CODES.to_vec();
    sorted.sort_unstable();
    let original_len = sorted.len();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        original_len,
        "DCS code table contains duplicates"
    );
}

// ============================================================================
// 4. Cross-reference tests (2 tests)
// ============================================================================

#[test]
fn tone_code_frequency_cross_reference() {
    for i in 0u8..50 {
        let tc = ToneCode::new(i).unwrap();
        let expected = CTCSS_FREQUENCIES[i as usize];
        let actual = tc.frequency_hz();
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "ToneCode({i}).frequency_hz() = {actual}, expected CTCSS_FREQUENCIES[{i}] = {expected}"
        );
    }
}

#[test]
fn dcs_code_value_cross_reference() {
    for i in 0u8..104 {
        let dc = DcsCode::new(i).unwrap();
        let expected = DCS_CODES[i as usize];
        let actual = dc.code_value();
        assert_eq!(
            actual, expected,
            "DcsCode({i}).code_value() = {actual}, expected DCS_CODES[{i}] = {expected}"
        );
    }
}

// ============================================================================
// 5. Error variant tests (1 test)
// ============================================================================

#[test]
fn all_validation_error_variants_display() {
    let variants: Vec<ValidationError> = vec![
        ValidationError::ToneCodeOutOfRange(99),
        ValidationError::BandOutOfRange(99),
        ValidationError::ModeOutOfRange(99),
        ValidationError::PowerLevelOutOfRange(99),
        ValidationError::ToneModeOutOfRange(99),
        ValidationError::ShiftOutOfRange(99),
        ValidationError::StepSizeOutOfRange(99),
        ValidationError::DataSpeedOutOfRange(99),
        ValidationError::LockoutOutOfRange(99),
        ValidationError::DcsCodeInvalid(99),
        ValidationError::ChannelNameTooLong { len: 99 },
        ValidationError::FrequencyOutOfRange(99),
    ];
    for (i, variant) in variants.iter().enumerate() {
        let msg = variant.to_string();
        assert!(
            !msg.is_empty(),
            "ValidationError variant {i} has empty display"
        );
    }
}

// ============================================================================
// 6. Round-trip tests (1 test)
// ============================================================================

#[test]
fn all_enum_types_round_trip() {
    // Band: 0..14
    for i in 0u8..14 {
        let val = Band::try_from(i).unwrap();
        assert_eq!(u8::from(val), i, "Band round-trip failed for {i}");
    }

    // Mode: 0..4
    for i in 0u8..4 {
        let val = Mode::try_from(i).unwrap();
        assert_eq!(u8::from(val), i, "Mode round-trip failed for {i}");
    }

    // PowerLevel: 0..4
    for i in 0u8..4 {
        let val = PowerLevel::try_from(i).unwrap();
        assert_eq!(u8::from(val), i, "PowerLevel round-trip failed for {i}");
    }

    // ToneMode: 0..4
    for i in 0u8..4 {
        let val = ToneMode::try_from(i).unwrap();
        assert_eq!(u8::from(val), i, "ToneMode round-trip failed for {i}");
    }

    // ShiftDirection: 0..16 (4-bit field)
    for i in 0u8..16 {
        let val = ShiftDirection::try_from(i).unwrap();
        assert_eq!(u8::from(val), i, "ShiftDirection round-trip failed for {i}");
    }

    // StepSize: 0..12
    for i in 0u8..12 {
        let val = StepSize::try_from(i).unwrap();
        assert_eq!(u8::from(val), i, "StepSize round-trip failed for {i}");
    }

    // DataSpeed: 0..2
    for i in 0u8..2 {
        let val = DataSpeed::try_from(i).unwrap();
        assert_eq!(u8::from(val), i, "DataSpeed round-trip failed for {i}");
    }

    // LockoutMode: 0..3
    for i in 0u8..3 {
        let val = LockoutMode::try_from(i).unwrap();
        assert_eq!(u8::from(val), i, "LockoutMode round-trip failed for {i}");
    }

    // CtcssMode: 0..3
    for i in 0u8..3 {
        let val = CtcssMode::try_from(i).unwrap();
        assert_eq!(u8::from(val), i, "CtcssMode round-trip failed for {i}");
    }
}
