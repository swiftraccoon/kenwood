//! Integration tests encoding every validation boundary from the firmware's
//! `radio_validate_channel_params` function at `0xC003C694`.
//!
//! These tests verify the exact accept/reject boundaries for each validated
//! type, ensuring 1:1 correspondence with firmware reverse-engineering data.

use kenwood_thd75::error::ValidationError;
use kenwood_thd75::types::tone::{CTCSS_FREQUENCIES, DCS_CODES};
use kenwood_thd75::types::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type BoxErr = Box<dyn std::error::Error>;

/// Return `table[idx]` or an error if the index is out of range.
fn entry<T: Copy>(table: &[T], idx: usize, name: &str) -> Result<T, BoxErr> {
    table
        .get(idx)
        .copied()
        .ok_or_else(|| format!("{name}[{idx}] out of range (len={})", table.len()).into())
}

// ============================================================================
// 1. Boundary tests — exact boundary for each validated type (12 tests)
// ============================================================================

#[test]
fn tone_code_boundary() {
    // Independent spec assertion — KI4LAX TABLE A (50 CTCSS) + ARFC RE (code 50 = 1750Hz)
    assert_eq!(ToneCode::MAX_INDEX, 50, "spec: KI4LAX TABLE A + ARFC RE");
    assert!(
        ToneCode::new(ToneCode::MAX_INDEX - 1).is_ok(),
        "49 is the last CTCSS tone code"
    );
    assert!(
        ToneCode::new(ToneCode::MAX_INDEX).is_ok(),
        "50 is the 1750 Hz tone burst (ARFC RE)"
    );
    assert!(
        ToneCode::new(ToneCode::MAX_INDEX + 1).is_err(),
        "51 must be rejected"
    );
}

#[test]
fn band_boundary() {
    assert_eq!(Band::COUNT, 14, "spec: firmware RE");
    assert!(
        Band::try_from(Band::COUNT - 1).is_ok(),
        "13 is the last valid band"
    );
    assert!(Band::try_from(Band::COUNT).is_err(), "14 must be rejected");
}

#[test]
fn mode_boundary() {
    assert_eq!(
        Mode::COUNT,
        10,
        "spec: KI4LAX TABLE D (0-7) + ARFC RE (8=WFM, 9=CW-R)"
    );
    assert!(Mode::try_from(7u8).is_ok(), "7 (DR) is valid");
    assert!(Mode::try_from(8u8).is_ok(), "8 (WFM) confirmed by ARFC RE");
    assert!(
        Mode::try_from(Mode::COUNT - 1).is_ok(),
        "9 (CW-R) confirmed by ARFC RE"
    );
    assert!(Mode::try_from(Mode::COUNT).is_err(), "10 must be rejected");
}

#[test]
fn power_level_boundary() {
    assert_eq!(PowerLevel::COUNT, 4, "spec: KI4LAX PC command");
    assert!(
        PowerLevel::try_from(PowerLevel::COUNT - 1).is_ok(),
        "3 (ExtraLow) is the last valid power level"
    );
    assert!(
        PowerLevel::try_from(PowerLevel::COUNT).is_err(),
        "4 must be rejected"
    );
}

#[test]
fn tone_mode_boundary() {
    assert_eq!(ToneMode::COUNT, 4, "spec: ARFC RE a/a1.cs");
    assert!(ToneMode::try_from(2u8).is_ok(), "2 (DCS) is valid");
    assert!(
        ToneMode::try_from(ToneMode::COUNT - 1).is_ok(),
        "3 (CrossTone) confirmed by ARFC RE"
    );
    assert!(
        ToneMode::try_from(ToneMode::COUNT).is_err(),
        "4 must be rejected"
    );
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
    assert_eq!(StepSize::COUNT, 12, "spec: KI4LAX TABLE C");
    assert!(
        StepSize::try_from(StepSize::COUNT - 1).is_ok(),
        "11 (100kHz) is the last valid step size"
    );
    assert!(
        StepSize::try_from(StepSize::COUNT).is_err(),
        "12 must be rejected"
    );
}

#[test]
fn data_speed_boundary() {
    assert_eq!(DataSpeed::COUNT, 2, "spec: KI4LAX");
    assert!(
        DataSpeed::try_from(DataSpeed::COUNT - 1).is_ok(),
        "1 (9600bps) is the last valid data speed"
    );
    assert!(
        DataSpeed::try_from(DataSpeed::COUNT).is_err(),
        "2 must be rejected"
    );
}

#[test]
fn lockout_boundary() {
    assert_eq!(LockoutMode::COUNT, 3, "spec: KI4LAX ME field");
    assert!(
        LockoutMode::try_from(LockoutMode::COUNT - 1).is_ok(),
        "2 (Group) is the last valid lockout mode"
    );
    assert!(
        LockoutMode::try_from(LockoutMode::COUNT).is_err(),
        "3 must be rejected"
    );
}

#[test]
fn ctcss_mode_boundary() {
    assert_eq!(CtcssMode::COUNT, 3, "spec: firmware RE");
    assert!(
        CtcssMode::try_from(CtcssMode::COUNT - 1).is_ok(),
        "2 (EncodeOnly) is the last valid CTCSS mode"
    );
    assert!(
        CtcssMode::try_from(CtcssMode::COUNT).is_err(),
        "3 must be rejected"
    );
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
    assert_eq!(DcsCode::COUNT, 104, "spec: KI4LAX TABLE B");
    assert_eq!(DcsCode::MAX_INDEX, 103, "spec: KI4LAX TABLE B");
    assert!(
        DcsCode::new(DcsCode::MAX_INDEX).is_ok(),
        "103 is the last valid DCS index"
    );
    assert!(
        DcsCode::new(DcsCode::COUNT).is_err(),
        "104 must be rejected"
    );
}

// ============================================================================
// 2. Full-range exhaustive tests (4 tests)
// ============================================================================

#[test]
fn tone_code_full_valid_range() -> TestResult {
    for i in 0u8..=ToneCode::MAX_INDEX {
        let val = ToneCode::new(i)?;
        assert_eq!(val.index(), i, "ToneCode round-trip failed at {i}");
    }
    for i in (ToneCode::MAX_INDEX + 1)..=255 {
        assert!(ToneCode::new(i).is_err(), "ToneCode({i}) should be invalid");
    }
    Ok(())
}

#[test]
fn band_full_valid_range() -> TestResult {
    for i in 0u8..Band::COUNT {
        let val = Band::try_from(i)?;
        assert_eq!(u8::from(val), i, "Band round-trip failed at {i}");
    }
    for i in Band::COUNT..=255 {
        assert!(Band::try_from(i).is_err(), "Band({i}) should be invalid");
    }
    Ok(())
}

#[test]
fn mode_full_valid_range() -> TestResult {
    for i in 0u8..Mode::COUNT {
        let val = Mode::try_from(i)?;
        assert_eq!(u8::from(val), i, "Mode round-trip failed at {i}");
    }
    for i in Mode::COUNT..=255 {
        assert!(Mode::try_from(i).is_err(), "Mode({i}) should be invalid");
    }
    Ok(())
}

#[test]
fn dcs_code_full_valid_range() -> TestResult {
    for i in 0u8..DcsCode::COUNT {
        let val = DcsCode::new(i)?;
        assert_eq!(val.index(), i, "DcsCode round-trip failed at {i}");
    }
    for i in DcsCode::COUNT..=255 {
        assert!(DcsCode::new(i).is_err(), "DcsCode({i}) should be invalid");
    }
    Ok(())
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
fn ctcss_table_monotonically_increasing() -> TestResult {
    for i in 1..CTCSS_FREQUENCIES.len() {
        let current = entry(&CTCSS_FREQUENCIES, i, "CTCSS_FREQUENCIES")?;
        let previous = entry(&CTCSS_FREQUENCIES, i - 1, "CTCSS_FREQUENCIES")?;
        assert!(
            current > previous,
            "CTCSS_FREQUENCIES[{i}] ({current}) must be > [{prev}] ({previous})",
            prev = i - 1,
        );
    }
    Ok(())
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
fn tone_code_frequency_cross_reference() -> TestResult {
    for i in 0u8..ToneCode::MAX_INDEX {
        let tc = ToneCode::new(i)?;
        let expected = entry(&CTCSS_FREQUENCIES, usize::from(i), "CTCSS_FREQUENCIES")?;
        let actual = tc.frequency_hz();
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "ToneCode({i}).frequency_hz() = {actual}, expected CTCSS_FREQUENCIES[{i}] = {expected}"
        );
    }
    Ok(())
}

#[test]
fn dcs_code_value_cross_reference() -> TestResult {
    for i in 0u8..DcsCode::COUNT {
        let dc = DcsCode::new(i)?;
        let expected = entry(&DCS_CODES, usize::from(i), "DCS_CODES")?;
        let actual = dc.code_value();
        assert_eq!(
            actual, expected,
            "DcsCode({i}).code_value() = {actual}, expected DCS_CODES[{i}] = {expected}"
        );
    }
    Ok(())
}

// ============================================================================
// 4b. Boundary tests for additional validated types (25 tests)
// ============================================================================

#[test]
fn squelch_level_boundary() {
    assert_eq!(SquelchLevel::COUNT, 7, "spec: KI4LAX SQ (0-6)");
    assert!(
        SquelchLevel::new(SquelchLevel::COUNT - 1).is_ok(),
        "6 is max"
    );
    assert!(
        SquelchLevel::new(SquelchLevel::COUNT).is_err(),
        "7 must be rejected"
    );
}

#[test]
fn smeter_reading_boundary() {
    assert_eq!(SMeterReading::COUNT, 6, "spec: KI4LAX SM (0-5)");
    assert!(
        SMeterReading::new(SMeterReading::COUNT - 1).is_ok(),
        "5 is max"
    );
    assert!(
        SMeterReading::new(SMeterReading::COUNT).is_err(),
        "6 must be rejected"
    );
}

#[test]
fn vfo_memory_mode_boundary() {
    assert_eq!(VfoMemoryMode::COUNT, 4, "spec: KI4LAX VM + ARFC RE");
    assert!(
        VfoMemoryMode::try_from(VfoMemoryMode::COUNT - 1).is_ok(),
        "3 is max"
    );
    assert!(
        VfoMemoryMode::try_from(VfoMemoryMode::COUNT).is_err(),
        "4 must be rejected"
    );
}

#[test]
fn filter_mode_boundary() {
    assert_eq!(FilterMode::COUNT, 3, "spec: KI4LAX SH");
    assert!(
        FilterMode::try_from(FilterMode::COUNT - 1).is_ok(),
        "2 is max"
    );
    assert!(
        FilterMode::try_from(FilterMode::COUNT).is_err(),
        "3 must be rejected"
    );
}

#[test]
fn battery_level_boundary() {
    assert_eq!(
        BatteryLevel::COUNT,
        5,
        "spec: KI4LAX BL (0-3) + HW verified (4=charging)"
    );
    assert!(
        BatteryLevel::try_from(BatteryLevel::COUNT - 1).is_ok(),
        "4 is max"
    );
    assert!(
        BatteryLevel::try_from(BatteryLevel::COUNT).is_err(),
        "5 must be rejected"
    );
}

#[test]
fn vox_gain_boundary() {
    assert_eq!(VoxGain::MAX, 9, "spec: User Manual Menu 151");
    assert!(VoxGain::new(VoxGain::MAX).is_ok(), "9 is max");
    assert!(
        VoxGain::new(VoxGain::MAX + 1).is_err(),
        "10 must be rejected"
    );
}

#[test]
fn vox_delay_boundary() {
    assert_eq!(VoxDelay::MAX, 30, "spec: User Manual Menu 152");
    assert!(VoxDelay::new(VoxDelay::MAX).is_ok(), "30 is max");
    assert!(
        VoxDelay::new(VoxDelay::MAX + 1).is_err(),
        "31 must be rejected"
    );
}

#[test]
fn tnc_baud_boundary() {
    assert_eq!(TncBaud::COUNT, 2, "spec: KI4LAX TN");
    assert!(TncBaud::try_from(TncBaud::COUNT - 1).is_ok(), "1 is max");
    assert!(
        TncBaud::try_from(TncBaud::COUNT).is_err(),
        "2 must be rejected"
    );
}

#[test]
fn beacon_mode_boundary() {
    assert_eq!(BeaconMode::COUNT, 5, "spec: User Manual");
    assert!(
        BeaconMode::try_from(BeaconMode::COUNT - 1).is_ok(),
        "4 is max"
    );
    assert!(
        BeaconMode::try_from(BeaconMode::COUNT).is_err(),
        "5 must be rejected"
    );
}

#[test]
fn dstar_slot_boundary() {
    assert_eq!(DstarSlot::MIN, 1, "spec: User Manual D-STAR");
    assert_eq!(DstarSlot::MAX, 6, "spec: User Manual D-STAR");
    assert!(
        DstarSlot::new(DstarSlot::MIN - 1).is_err(),
        "0 must be rejected (min is 1)"
    );
    assert!(DstarSlot::new(DstarSlot::MIN).is_ok(), "1 is min");
    assert!(DstarSlot::new(DstarSlot::MAX).is_ok(), "6 is max");
    assert!(
        DstarSlot::new(DstarSlot::MAX + 1).is_err(),
        "7 must be rejected"
    );
}

#[test]
fn callsign_slot_boundary() {
    assert_eq!(CallsignSlot::MAX, 10, "spec: firmware RE");
    assert!(CallsignSlot::new(CallsignSlot::MAX).is_ok(), "10 is max");
    assert!(
        CallsignSlot::new(CallsignSlot::MAX + 1).is_err(),
        "11 must be rejected"
    );
}

#[test]
fn detect_output_mode_boundary() {
    assert_eq!(DetectOutputMode::COUNT, 3, "spec: KI4LAX IO + ARFC RE");
    assert!(
        DetectOutputMode::try_from(DetectOutputMode::COUNT - 1).is_ok(),
        "2 is max"
    );
    assert!(
        DetectOutputMode::try_from(DetectOutputMode::COUNT).is_err(),
        "3 must be rejected"
    );
}

#[test]
fn dv_gateway_mode_boundary() {
    assert_eq!(DvGatewayMode::COUNT, 3, "spec: ARFC RE a/ai.cs");
    assert!(
        DvGatewayMode::try_from(DvGatewayMode::COUNT - 1).is_ok(),
        "2 is max"
    );
    assert!(
        DvGatewayMode::try_from(DvGatewayMode::COUNT).is_err(),
        "3 must be rejected"
    );
}

#[test]
fn tnc_mode_boundary() {
    assert_eq!(
        TncMode::COUNT,
        4,
        "spec: firmware RE (3 from KI4LAX + MMDVM)"
    );
    assert!(TncMode::try_from(TncMode::COUNT - 1).is_ok(), "3 is max");
    assert!(
        TncMode::try_from(TncMode::COUNT).is_err(),
        "4 must be rejected"
    );
}

#[test]
fn gps_radio_mode_boundary() {
    assert_eq!(GpsRadioMode::COUNT, 2, "spec: firmware guard local_18 < 2");
    assert!(
        GpsRadioMode::try_from(GpsRadioMode::COUNT - 1).is_ok(),
        "1 is max"
    );
    assert!(
        GpsRadioMode::try_from(GpsRadioMode::COUNT).is_err(),
        "2 must be rejected"
    );
}

#[test]
fn filter_width_index_ssb_boundary() {
    assert!(
        FilterWidthIndex::new(4, FilterMode::Ssb).is_ok(),
        "4 is max for SSB"
    );
    assert!(
        FilterWidthIndex::new(5, FilterMode::Ssb).is_err(),
        "5 must be rejected for SSB"
    );
}

#[test]
fn filter_width_index_am_boundary() {
    assert!(
        FilterWidthIndex::new(3, FilterMode::Am).is_ok(),
        "3 is max for AM"
    );
    assert!(
        FilterWidthIndex::new(4, FilterMode::Am).is_err(),
        "4 must be rejected for AM"
    );
}

#[test]
fn fine_step_boundary() {
    assert_eq!(FineStep::COUNT, 4, "spec: KI4LAX TABLE E");
    assert!(FineStep::try_from(FineStep::COUNT - 1).is_ok(), "3 is max");
    assert!(
        FineStep::try_from(FineStep::COUNT).is_err(),
        "4 must be rejected"
    );
}

#[test]
fn key_lock_type_boundary() {
    assert_eq!(KeyLockType::COUNT, 3, "spec: User Manual Menu 960");
    assert!(
        KeyLockType::try_from(KeyLockType::COUNT - 1).is_ok(),
        "2 is max"
    );
    assert!(
        KeyLockType::try_from(KeyLockType::COUNT).is_err(),
        "3 must be rejected"
    );
}

#[test]
fn coarse_step_multiplier_boundary() {
    assert_eq!(CoarseStepMultiplier::COUNT, 6, "spec: ARFC RE a/a2.cs");
    assert!(
        CoarseStepMultiplier::try_from(CoarseStepMultiplier::COUNT - 1).is_ok(),
        "5 is max"
    );
    assert!(
        CoarseStepMultiplier::try_from(CoarseStepMultiplier::COUNT).is_err(),
        "6 must be rejected"
    );
}

#[test]
fn cross_tone_type_boundary() {
    assert_eq!(CrossToneType::COUNT, 4, "spec: User Manual Chapter 10");
    assert!(
        CrossToneType::try_from(CrossToneType::COUNT - 1).is_ok(),
        "3 is max"
    );
    assert!(
        CrossToneType::try_from(CrossToneType::COUNT).is_err(),
        "4 must be rejected"
    );
}

#[test]
fn flash_duplex_boundary() {
    assert_eq!(FlashDuplex::COUNT, 3, "spec: firmware RE byte 0x0A");
    assert!(
        FlashDuplex::try_from(FlashDuplex::COUNT - 1).is_ok(),
        "2 is max"
    );
    assert!(
        FlashDuplex::try_from(FlashDuplex::COUNT).is_err(),
        "3 must be rejected"
    );
}

#[test]
fn flash_digital_squelch_boundary() {
    assert_eq!(FlashDigitalSquelch::COUNT, 3, "spec: KI4LAX FO field T");
    assert!(
        FlashDigitalSquelch::try_from(FlashDigitalSquelch::COUNT - 1).is_ok(),
        "2 is max"
    );
    assert!(
        FlashDigitalSquelch::try_from(FlashDigitalSquelch::COUNT).is_err(),
        "3 must be rejected"
    );
}

#[test]
fn memory_mode_boundary() {
    assert_eq!(MemoryMode::COUNT, 8, "spec: KI4LAX TABLE D");
    assert!(
        MemoryMode::try_from(MemoryMode::COUNT - 1).is_ok(),
        "7 is max"
    );
    assert!(
        MemoryMode::try_from(MemoryMode::COUNT).is_err(),
        "8 must be rejected"
    );
}

#[test]
fn scan_resume_method_boundary() {
    assert_eq!(ScanResumeMethod::COUNT, 3, "spec: User Manual Menu 130/131");
    assert!(
        ScanResumeMethod::try_from(ScanResumeMethod::COUNT - 1).is_ok(),
        "2 is max"
    );
    assert!(
        ScanResumeMethod::try_from(ScanResumeMethod::COUNT).is_err(),
        "3 must be rejected"
    );
}

#[test]
fn cw_pitch_boundary() {
    assert!(CwPitch::new(400).is_some(), "400 is min");
    assert!(CwPitch::new(1000).is_some(), "1000 is max");
    assert!(CwPitch::new(399).is_none(), "399 must be rejected");
    assert!(CwPitch::new(1001).is_none(), "1001 must be rejected");
    assert!(
        CwPitch::new(450).is_none(),
        "450 must be rejected (not on 100 Hz step)"
    );
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
// 6. Round-trip tests — channel/tone types
// ============================================================================

/// Verify `TryFrom<u8>` -> `From<T>` -> `u8` round-trip for a range.
fn assert_try_from_round_trip<T>(range: std::ops::Range<u8>, name: &str) -> Result<(), BoxErr>
where
    T: TryFrom<u8> + Copy,
    u8: From<T>,
    <T as TryFrom<u8>>::Error: std::fmt::Debug,
{
    for i in range {
        let val = T::try_from(i)
            .map_err(|e| -> BoxErr { format!("{name}({i}) should be valid: {e:?}").into() })?;
        assert_eq!(u8::from(val), i, "{name} round-trip failed for {i}");
    }
    Ok(())
}

/// Verify `TryFrom<u8>` -> `From<T>` -> `u8` round-trip for an inclusive range.
fn assert_try_from_round_trip_inclusive<T>(
    range: std::ops::RangeInclusive<u8>,
    name: &str,
) -> Result<(), BoxErr>
where
    T: TryFrom<u8> + Copy,
    u8: From<T>,
    <T as TryFrom<u8>>::Error: std::fmt::Debug,
{
    for i in range {
        let val = T::try_from(i)
            .map_err(|e| -> BoxErr { format!("{name}({i}) should be valid: {e:?}").into() })?;
        assert_eq!(u8::from(val), i, "{name} round-trip failed for {i}");
    }
    Ok(())
}

#[test]
fn channel_enum_types_round_trip() -> TestResult {
    assert_try_from_round_trip::<Band>(0..Band::COUNT, "Band")?;
    assert_try_from_round_trip::<Mode>(0..Mode::COUNT, "Mode")?;
    assert_try_from_round_trip::<PowerLevel>(0..PowerLevel::COUNT, "PowerLevel")?;
    assert_try_from_round_trip::<ToneMode>(0..ToneMode::COUNT, "ToneMode")?;
    assert_try_from_round_trip::<ShiftDirection>(0..16, "ShiftDirection")?;
    assert_try_from_round_trip::<StepSize>(0..StepSize::COUNT, "StepSize")?;
    assert_try_from_round_trip::<DataSpeed>(0..DataSpeed::COUNT, "DataSpeed")?;
    assert_try_from_round_trip::<LockoutMode>(0..LockoutMode::COUNT, "LockoutMode")?;
    assert_try_from_round_trip::<CtcssMode>(0..CtcssMode::COUNT, "CtcssMode")?;
    assert_try_from_round_trip::<CrossToneType>(0..CrossToneType::COUNT, "CrossToneType")?;
    assert_try_from_round_trip::<FlashDuplex>(0..FlashDuplex::COUNT, "FlashDuplex")?;
    assert_try_from_round_trip::<FlashDigitalSquelch>(
        0..FlashDigitalSquelch::COUNT,
        "FlashDigitalSquelch",
    )?;
    assert_try_from_round_trip::<FineStep>(0..FineStep::COUNT, "FineStep")?;
    assert_try_from_round_trip::<MemoryMode>(0..MemoryMode::COUNT, "MemoryMode")?;
    Ok(())
}

// ============================================================================
// 7. Round-trip tests — radio parameter types
// ============================================================================

#[test]
fn radio_param_types_round_trip() -> TestResult {
    assert_try_from_round_trip::<SquelchLevel>(0..SquelchLevel::COUNT, "SquelchLevel")?;
    assert_try_from_round_trip::<SMeterReading>(0..SMeterReading::COUNT, "SMeterReading")?;
    assert_try_from_round_trip::<VfoMemoryMode>(0..VfoMemoryMode::COUNT, "VfoMemoryMode")?;
    assert_try_from_round_trip::<FilterMode>(0..FilterMode::COUNT, "FilterMode")?;
    assert_try_from_round_trip::<BatteryLevel>(0..BatteryLevel::COUNT, "BatteryLevel")?;
    assert_try_from_round_trip_inclusive::<VoxGain>(0..=VoxGain::MAX, "VoxGain")?;
    assert_try_from_round_trip_inclusive::<VoxDelay>(0..=VoxDelay::MAX, "VoxDelay")?;
    assert_try_from_round_trip::<TncBaud>(0..TncBaud::COUNT, "TncBaud")?;
    assert_try_from_round_trip::<BeaconMode>(0..BeaconMode::COUNT, "BeaconMode")?;
    assert_try_from_round_trip::<DetectOutputMode>(0..DetectOutputMode::COUNT, "DetectOutputMode")?;
    assert_try_from_round_trip::<DvGatewayMode>(0..DvGatewayMode::COUNT, "DvGatewayMode")?;
    assert_try_from_round_trip::<TncMode>(0..TncMode::COUNT, "TncMode")?;
    assert_try_from_round_trip::<GpsRadioMode>(0..GpsRadioMode::COUNT, "GpsRadioMode")?;
    assert_try_from_round_trip::<FilterWidthIndex>(0..5, "FilterWidthIndex")?;
    assert_try_from_round_trip::<KeyLockType>(0..KeyLockType::COUNT, "KeyLockType")?;
    assert_try_from_round_trip::<CoarseStepMultiplier>(
        0..CoarseStepMultiplier::COUNT,
        "CoarseStepMultiplier",
    )?;

    // DstarSlot starts at 1
    for i in DstarSlot::MIN..=DstarSlot::MAX {
        let val = DstarSlot::new(i)?;
        assert_eq!(u8::from(val), i, "DstarSlot round-trip failed for {i}");
    }

    // CallsignSlot: 0..=MAX
    for i in 0u8..=CallsignSlot::MAX {
        let val = CallsignSlot::new(i)?;
        assert_eq!(u8::from(val), i, "CallsignSlot round-trip failed for {i}");
    }

    // ScanResumeMethod uses from_raw/to_raw
    for i in 0u8..ScanResumeMethod::COUNT {
        let val = ScanResumeMethod::from_raw(i).ok_or_else(|| -> BoxErr {
            format!("ScanResumeMethod::from_raw({i}) returned None").into()
        })?;
        assert_eq!(
            val.to_raw(),
            i,
            "ScanResumeMethod round-trip failed for {i}"
        );
    }
    Ok(())
}
