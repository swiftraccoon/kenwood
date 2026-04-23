//! Property-based round-trip tests for the CAT protocol.
//!
//! Uses `proptest` to verify that serialize-then-parse produces the
//! original values, and that binary packing round-trips exactly.

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::tone::{CtcssMode, DcsCode, ToneCode};
use kenwood_thd75::types::*;
use kiss_tnc::{KissFrame, decode_kiss_frame, encode_kiss_frame};

/// Convert any debug-printable error into a `TestCaseError` so `?` can be used
/// in proptest blocks without violating workspace `unwrap_used` policy.
fn to_test_err<E: std::fmt::Debug>(e: E) -> TestCaseError {
    TestCaseError::fail(format!("{e:?}"))
}

// ============================================================================
// Arbitrary strategies
// ============================================================================

fn arb_band() -> impl Strategy<Value = Band> {
    // Only A/B for protocol tests. Try_from will not fail — the range 0..2 is
    // within Band::COUNT. Converting the panic to an `Option::filter` keeps
    // proptest from panicking if someone widens the range and hits an invalid
    // value; `.prop_filter_map` would hide the bug but this unwrap is documented
    // to be unreachable.
    (0u8..2).prop_filter_map("invalid band", |i| Band::try_from(i).ok())
}

fn arb_channel_memory() -> impl Strategy<Value = ChannelMemory> {
    // Split into sub-tuples to stay within proptest's 12-element limit.
    // flags_0a_raw is the source of truth for tone/shift wire fields.
    // Individual bool fields (tone_enable, dcs_enable, etc.) and shift must
    // be consistent with flags_0a_raw for serialize→parse round-trip.
    let part_a = (
        any::<u32>(),            // rx_frequency
        any::<u32>(),            // tx_offset
        (0u8..StepSize::COUNT),  // step_size
        (0u8..CtcssMode::COUNT), // ctcss_mode
        (0u8..=255u8),           // flags_0a_raw (all 8 bits — serializer uses this)
    );
    let part_b = (
        (0u8..ToneCode::MAX_INDEX),        // tone_code
        (0u8..ToneCode::MAX_INDEX),        // ctcss_code
        (0u8..DcsCode::COUNT),             // dcs_code
        (0u8..CrossToneType::COUNT),       // cross_tone_combo
        (0u8..FlashDigitalSquelch::COUNT), // digital_squelch
        "[A-Z0-9]{0,8}",                   // urcall (alphanumeric only for wire safety)
        any::<u8>(),                       // data_mode
    );
    (part_a, part_b).prop_filter_map(
        "channel memory construction failed",
        |((rx, tx, step, _ctcss_m, flags), (tc, cc, dc, ds, lo, urcall, dm))| {
            // Derive individual fields from flags_0a_raw for consistency
            let tone_enable = (flags >> 7) & 1 != 0;
            let ctcss_enable = (flags >> 6) & 1 != 0;
            let dcs_enable = (flags >> 5) & 1 != 0;
            let cross_tone = (flags >> 4) & 1 != 0;
            let reverse = (flags >> 3) & 1 != 0;
            let shift_val = flags & 0x07;
            Some(ChannelMemory {
                rx_frequency: Frequency::new(rx),
                tx_offset: Frequency::new(tx),
                step_size: StepSize::try_from(step).ok()?,
                mode_flags_raw: 0,
                shift: ShiftDirection::try_from(shift_val).ok()?,
                reverse,
                tone_enable,
                ctcss_mode: CtcssMode::try_from(u8::from(ctcss_enable)).ok()?,
                dcs_enable,
                cross_tone_reverse: cross_tone,
                flags_0a_raw: flags,
                tone_code: ToneCode::new(tc).ok()?,
                ctcss_code: ToneCode::new(cc).ok()?,
                dcs_code: DcsCode::new(dc).ok()?,
                cross_tone_combo: CrossToneType::try_from(ds).ok()?,
                digital_squelch: FlashDigitalSquelch::try_from(lo).ok()?,
                urcall: ChannelName::new(&urcall).ok()?,
                data_mode: dm,
            })
        },
    )
}

// ============================================================================
// Property-based tests
// ============================================================================

proptest! {
    // 1. FO wire round-trip: serialize SetFrequencyFull, parse as FrequencyFull
    #[test]
    fn fo_wire_round_trip(band in arb_band(), channel in arb_channel_memory()) {
        let cmd = Command::SetFrequencyFull { band, channel: channel.clone() };
        let wire = protocol::serialize(&cmd);
        let frame = wire.split_last().map(|(_, rest)| rest).ok_or_else(|| to_test_err("empty wire"))?;
        let response = protocol::parse(frame).map_err(to_test_err)?;
        let Response::FrequencyFull { band: b, channel: parsed } = response else {
            prop_assert!(false, "wrong variant: {response:?}");
            return Ok(());
        };
        prop_assert_eq!(b, band);
        prop_assert_eq!(parsed, channel);
    }

    // 2. 40-byte binary round-trip (byte[10] mapping now matches hardware)
    #[test]
    fn channel_memory_40byte_round_trip(channel in arb_channel_memory()) {
        let bytes = channel.to_bytes();
        prop_assert_eq!(bytes.len(), 40);
        let parsed = ChannelMemory::from_bytes(&bytes).map_err(to_test_err)?;
        prop_assert_eq!(parsed, channel);
    }

    // 3. Byte 0x08 packing (step + shift)
    #[test]
    fn byte_08_packing(step in 0u8..StepSize::COUNT, shift in 0u8..4) {
        let ch = ChannelMemory {
            step_size: StepSize::try_from(step).map_err(to_test_err)?,
            mode_flags_raw: 0,
            shift: ShiftDirection::try_from(shift).map_err(to_test_err)?,
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        let b08 = *bytes.get(0x08).ok_or_else(|| to_test_err("bytes[0x08] missing"))?;
        prop_assert_eq!(b08 >> 4, step);
        prop_assert_eq!(b08 & 0x0F, shift);
    }

    // 4. Byte 0x09 packing (currently zeroed — mode/fine not individually modeled)
    #[test]
    fn byte_09_packing(_rev in any::<bool>(), _tone in any::<bool>(), _ctcss in 0u8..CtcssMode::COUNT) {
        let ch = ChannelMemory::default();
        let bytes = ch.to_bytes();
        let b09 = *bytes.get(0x09).ok_or_else(|| to_test_err("bytes[0x09] missing"))?;
        prop_assert_eq!(b09, 0);
    }

    // 5. Byte 0x0A packing — flags_0a_raw is stored directly (hardware-verified)
    #[test]
    fn byte_0a_packing(flags in 0u8..=255u8) {
        let ch = ChannelMemory {
            flags_0a_raw: flags,
            // Derive individual fields for struct consistency
            tone_enable: (flags >> 7) & 1 != 0,
            dcs_enable: (flags >> 5) & 1 != 0,
            cross_tone_reverse: (flags >> 4) & 1 != 0,
            reverse: (flags >> 3) & 1 != 0,
            shift: ShiftDirection::try_from(flags & 0x07).map_err(to_test_err)?,
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        let b0a = *bytes.get(0x0A).ok_or_else(|| to_test_err("bytes[0x0A] missing"))?;
        prop_assert_eq!(b0a, flags);
    }

    // 6. Byte 0x0E packing (cross_tone_combo + digital_squelch)
    #[test]
    fn byte_0e_packing(combo in 0u8..CrossToneType::COUNT, squelch in 0u8..FlashDigitalSquelch::COUNT) {
        let ch = ChannelMemory {
            cross_tone_combo: CrossToneType::try_from(combo).map_err(to_test_err)?,
            digital_squelch: FlashDigitalSquelch::try_from(squelch).map_err(to_test_err)?,
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        let b0e = *bytes.get(0x0E).ok_or_else(|| to_test_err("bytes[0x0E] missing"))?;
        prop_assert_eq!((b0e >> 4) & 0x03, combo);
        prop_assert_eq!(b0e & 0x03, squelch);
    }

    // 7. Frequency wire format round-trip
    #[test]
    fn frequency_wire_round_trip(hz in any::<u32>()) {
        let f = Frequency::new(hz);
        let wire = f.to_wire_string();
        let parsed = Frequency::from_wire_string(&wire).map_err(to_test_err)?;
        prop_assert_eq!(parsed.as_hz(), hz);
    }

    // 8. Frequency LE bytes round-trip
    #[test]
    fn frequency_le_round_trip(hz in any::<u32>()) {
        let f = Frequency::new(hz);
        let parsed = Frequency::from_le_bytes(f.to_le_bytes());
        prop_assert_eq!(parsed.as_hz(), hz);
    }

    // 9. ToneCode round-trip
    #[test]
    fn tone_code_round_trip(idx in 0u8..=ToneCode::MAX_INDEX) {
        let tc = ToneCode::new(idx).map_err(to_test_err)?;
        prop_assert_eq!(tc.index(), idx);
    }

    // 10. DcsCode round-trip
    #[test]
    fn dcs_code_round_trip(idx in 0u8..DcsCode::COUNT) {
        let dc = DcsCode::new(idx).map_err(to_test_err)?;
        prop_assert_eq!(dc.index(), idx);
    }

    // 11. AG (AF gain) — no round-trip: write is band-indexed "AG band,level"
    //     but read returns bare "AG level" (no band). Asymmetric by design.

    // 12. SQ (squelch) wire round-trip
    #[test]
    fn sq_round_trip(band in arb_band(), raw_level in 0u8..SquelchLevel::COUNT) {
        let level = SquelchLevel::new(raw_level).map_err(to_test_err)?;
        let cmd = Command::SetSquelch { band, level };
        let wire = protocol::serialize(&cmd);
        let frame = wire.split_last().map(|(_, rest)| rest).ok_or_else(|| to_test_err("empty wire"))?;
        let r = protocol::parse(frame).map_err(to_test_err)?;
        let Response::Squelch { band: b, level: l } = r else {
            prop_assert!(false, "wrong: {r:?}");
            return Ok(());
        };
        prop_assert_eq!(b, band);
        prop_assert_eq!(l, level);
    }

    // 13. MD (mode) wire round-trip
    #[test]
    fn md_round_trip(band in arb_band(), mode_val in 0u8..Mode::COUNT) {
        let mode = Mode::try_from(mode_val).map_err(to_test_err)?;
        let cmd = Command::SetMode { band, mode };
        let wire = protocol::serialize(&cmd);
        let frame = wire.split_last().map(|(_, rest)| rest).ok_or_else(|| to_test_err("empty wire"))?;
        let r = protocol::parse(frame).map_err(to_test_err)?;
        let Response::Mode { band: b, mode: m } = r else {
            prop_assert!(false, "wrong: {r:?}");
            return Ok(());
        };
        prop_assert_eq!(b, band);
        prop_assert_eq!(m, mode);
    }

    // 14. PC (power level) wire round-trip
    #[test]
    fn pc_round_trip(band in arb_band(), pl in 0u8..PowerLevel::COUNT) {
        let level = PowerLevel::try_from(pl).map_err(to_test_err)?;
        let cmd = Command::SetPowerLevel { band, level };
        let wire = protocol::serialize(&cmd);
        let frame = wire.split_last().map(|(_, rest)| rest).ok_or_else(|| to_test_err("empty wire"))?;
        let r = protocol::parse(frame).map_err(to_test_err)?;
        let Response::PowerLevel { band: b, level: l } = r else {
            prop_assert!(false, "wrong: {r:?}");
            return Ok(());
        };
        prop_assert_eq!(b, band);
        prop_assert_eq!(l, level);
    }

    // 16. KISS frame encode/decode round-trip
    //
    // The type byte (port<<4 | command) is NOT escaped in KISS framing,
    // so combinations producing 0xC0 (FEND) or 0xDB (FESC) as the type
    // byte cannot round-trip. We restrict to port 0 (the only port the
    // TH-D75 uses) and command 0..7 (the defined KISS commands), which
    // keeps the type byte in the safe 0x00-0x07 range.
    #[test]
    fn kiss_frame_round_trip(command in 0u8..7, data in proptest::collection::vec(any::<u8>(), 0..100)) {
        let frame = KissFrame { port: 0, command, data };
        let encoded = encode_kiss_frame(&frame);
        let decoded = decode_kiss_frame(&encoded).map_err(to_test_err)?;
        prop_assert_eq!(decoded, frame);
    }

    // TN (TNC mode) is a bare read command — no write variant, so no round-trip.
    // CTCSS tone is configured through the FO (full channel) command.

    // 15. ME wire round-trip: serialize SetMemoryChannel, parse as MemoryChannel
    #[test]
    fn me_wire_round_trip(ch_num in 0u16..1000, channel in arb_channel_memory()) {
        let cmd = Command::SetMemoryChannel { channel: ch_num, data: channel.clone() };
        let wire = protocol::serialize(&cmd);
        let frame = wire.split_last().map(|(_, rest)| rest).ok_or_else(|| to_test_err("empty wire"))?;
        let response = protocol::parse(frame).map_err(to_test_err)?;
        let Response::MemoryChannel { channel: num, data: parsed } = response else {
            prop_assert!(false, "wrong variant: {response:?}");
            return Ok(());
        };
        prop_assert_eq!(num, ch_num);
        prop_assert_eq!(parsed, channel);
    }

    // 17. GW (DV Gateway mode) wire round-trip
    #[test]
    fn gw_round_trip(gw_val in 0u8..DvGatewayMode::COUNT) {
        let value = DvGatewayMode::try_from(gw_val).map_err(to_test_err)?;
        let cmd = Command::SetGateway { value };
        let wire = protocol::serialize(&cmd);
        let frame = wire.split_last().map(|(_, rest)| rest).ok_or_else(|| to_test_err("empty wire"))?;
        let r = protocol::parse(frame).map_err(to_test_err)?;
        let Response::Gateway { value: v } = r else {
            prop_assert!(false, "wrong: {r:?}");
            return Ok(());
        };
        prop_assert_eq!(v, value);
    }

    // 18. SH (filter width) wire round-trip
    #[test]
    fn sh_round_trip(mode in 0u8..FilterMode::COUNT, width in 0u8..5) {
        let filter_mode = FilterMode::try_from(mode).map_err(to_test_err)?;
        // AM mode max is 3, SSB/CW max is 4
        let max_width = if mode == 2 { 4 } else { 5 };
        if width < max_width {
            let filter_width = FilterWidthIndex::new(width, filter_mode).map_err(to_test_err)?;
            let cmd = Command::SetFilterWidth { mode: filter_mode, width: filter_width };
            let wire = protocol::serialize(&cmd);
            let frame = wire.split_last().map(|(_, rest)| rest).ok_or_else(|| to_test_err("empty wire"))?;
            let r = protocol::parse(frame).map_err(to_test_err)?;
            let Response::FilterWidth { mode: m, width: w } = r else {
                prop_assert!(false, "wrong: {r:?}");
                return Ok(());
            };
            prop_assert_eq!(m, filter_mode);
            prop_assert_eq!(w, filter_width);
        }
    }
}
