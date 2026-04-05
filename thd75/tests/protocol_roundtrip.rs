//! Property-based round-trip tests for the CAT protocol.
//!
//! Uses `proptest` to verify that serialize-then-parse produces the
//! original values, and that binary packing round-trips exactly.

use proptest::prelude::*;

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::tone::{CtcssMode, DcsCode, ToneCode};
use kenwood_thd75::types::*;

// ============================================================================
// Arbitrary strategies
// ============================================================================

fn arb_band() -> impl Strategy<Value = Band> {
    (0u8..2).prop_map(|i| Band::try_from(i).unwrap()) // Only A/B for protocol tests
}

fn arb_channel_memory() -> impl Strategy<Value = ChannelMemory> {
    // Split into sub-tuples to stay within proptest's 12-element limit.
    // flags_0a_raw is the source of truth for tone/shift wire fields.
    // Individual bool fields (tone_enable, dcs_enable, etc.) and shift must
    // be consistent with flags_0a_raw for serialize→parse round-trip.
    let part_a = (
        any::<u32>(),  // rx_frequency
        any::<u32>(),  // tx_offset
        (0u8..12),     // step_size
        (0u8..3),      // ctcss_mode
        (0u8..=255u8), // flags_0a_raw (all 8 bits — serializer uses this)
    );
    let part_b = (
        (0u8..50),       // tone_code
        (0u8..50),       // ctcss_code
        (0u8..104),      // dcs_code
        (0u8..4),        // cross_tone_combo
        (0u8..3),        // digital_squelch
        "[A-Z0-9]{0,8}", // urcall (alphanumeric only for wire safety)
        any::<u8>(),     // data_mode
    );
    (part_a, part_b).prop_map(
        |((rx, tx, step, ctcss_m, flags), (tc, cc, dc, ds, lo, urcall, dm))| {
            // Derive individual fields from flags_0a_raw for consistency
            let tone_enable = (flags >> 7) & 1 != 0;
            let ctcss_enable = (flags >> 6) & 1 != 0;
            let dcs_enable = (flags >> 5) & 1 != 0;
            let cross_tone = (flags >> 4) & 1 != 0;
            let reverse = (flags >> 3) & 1 != 0;
            let shift_val = flags & 0x07;
            ChannelMemory {
                rx_frequency: Frequency::new(rx),
                tx_offset: Frequency::new(tx),
                step_size: StepSize::try_from(step).unwrap(),
                mode_flags_raw: 0,
                shift: ShiftDirection::try_from(shift_val).unwrap(),
                reverse,
                tone_enable,
                ctcss_mode: if ctcss_enable {
                    CtcssMode::try_from(1u8).unwrap()
                } else {
                    CtcssMode::try_from(0u8).unwrap()
                },
                dcs_enable,
                cross_tone_reverse: cross_tone,
                flags_0a_raw: flags,
                tone_code: ToneCode::new(tc).unwrap(),
                ctcss_code: ToneCode::new(cc).unwrap(),
                dcs_code: DcsCode::new(dc).unwrap(),
                cross_tone_combo: CrossToneType::try_from(ds).unwrap(),
                digital_squelch: FlashDigitalSquelch::try_from(lo).unwrap(),
                urcall: ChannelName::new(&urcall).unwrap(),
                data_mode: dm,
            }
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
        let frame = &wire[..wire.len() - 1]; // strip \r
        let response = protocol::parse(frame).unwrap();
        match response {
            Response::FrequencyFull { band: b, channel: parsed } => {
                prop_assert_eq!(b, band);
                prop_assert_eq!(parsed, channel);
            }
            other => prop_assert!(false, "wrong variant: {other:?}"),
        }
    }

    // 2. 40-byte binary round-trip (byte[10] mapping now matches hardware)
    #[test]
    fn channel_memory_40byte_round_trip(channel in arb_channel_memory()) {
        let bytes = channel.to_bytes();
        prop_assert_eq!(bytes.len(), 40);
        let parsed = ChannelMemory::from_bytes(&bytes).unwrap();
        prop_assert_eq!(parsed, channel);
    }

    // 3. Byte 0x08 packing (step + shift)
    #[test]
    fn byte_08_packing(step in 0u8..12, shift in 0u8..4) {
        let ch = ChannelMemory {
            step_size: StepSize::try_from(step).unwrap(),
                mode_flags_raw: 0,
            shift: ShiftDirection::try_from(shift).unwrap(),
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        prop_assert_eq!(bytes[0x08] >> 4, step);
        prop_assert_eq!(bytes[0x08] & 0x0F, shift);
    }

    // 4. Byte 0x09 packing (currently zeroed — mode/fine not individually modeled)
    #[test]
    fn byte_09_packing(_rev in any::<bool>(), _tone in any::<bool>(), _ctcss in 0u8..3) {
        let ch = ChannelMemory::default();
        let bytes = ch.to_bytes();
        prop_assert_eq!(bytes[0x09], 0);
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
            shift: ShiftDirection::try_from(flags & 0x07).unwrap(),
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        prop_assert_eq!(bytes[0x0A], flags);
    }

    // 6. Byte 0x0E packing (cross_tone_combo + digital_squelch)
    #[test]
    fn byte_0e_packing(combo in 0u8..4, squelch in 0u8..3) {
        let ch = ChannelMemory {
            cross_tone_combo: CrossToneType::try_from(combo).unwrap(),
            digital_squelch: FlashDigitalSquelch::try_from(squelch).unwrap(),
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        prop_assert_eq!((bytes[0x0E] >> 4) & 0x03, combo);
        prop_assert_eq!(bytes[0x0E] & 0x03, squelch);
    }

    // 7. Frequency wire format round-trip
    #[test]
    fn frequency_wire_round_trip(hz in any::<u32>()) {
        let f = Frequency::new(hz);
        let wire = f.to_wire_string();
        let parsed = Frequency::from_wire_string(&wire).unwrap();
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
    fn tone_code_round_trip(idx in 0u8..50) {
        let tc = ToneCode::new(idx).unwrap();
        prop_assert_eq!(tc.index(), idx);
    }

    // 10. DcsCode round-trip
    #[test]
    fn dcs_code_round_trip(idx in 0u8..104) {
        let dc = DcsCode::new(idx).unwrap();
        prop_assert_eq!(dc.index(), idx);
    }

    // 11. AG (AF gain) — no round-trip: write is band-indexed "AG band,level"
    //     but read returns bare "AG level" (no band). Asymmetric by design.

    // 12. SQ (squelch) wire round-trip
    #[test]
    fn sq_round_trip(band in arb_band(), raw_level in 0u8..7) {
        let level = SquelchLevel::new(raw_level).unwrap();
        let cmd = Command::SetSquelch { band, level };
        let wire = protocol::serialize(&cmd);
        let frame = &wire[..wire.len() - 1];
        match protocol::parse(frame).unwrap() {
            Response::Squelch { band: b, level: l } => {
                prop_assert_eq!(b, band);
                prop_assert_eq!(l, level);
            }
            other => prop_assert!(false, "wrong: {other:?}"),
        }
    }

    // 13. MD (mode) wire round-trip
    #[test]
    fn md_round_trip(band in arb_band(), mode_val in 0u8..4) {
        let mode = Mode::try_from(mode_val).unwrap();
        let cmd = Command::SetMode { band, mode };
        let wire = protocol::serialize(&cmd);
        let frame = &wire[..wire.len() - 1];
        match protocol::parse(frame).unwrap() {
            Response::Mode { band: b, mode: m } => {
                prop_assert_eq!(b, band);
                prop_assert_eq!(m, mode);
            }
            other => prop_assert!(false, "wrong: {other:?}"),
        }
    }

    // 14. PC (power level) wire round-trip
    #[test]
    fn pc_round_trip(band in arb_band(), pl in 0u8..4) {
        let level = PowerLevel::try_from(pl).unwrap();
        let cmd = Command::SetPowerLevel { band, level };
        let wire = protocol::serialize(&cmd);
        let frame = &wire[..wire.len() - 1];
        match protocol::parse(frame).unwrap() {
            Response::PowerLevel { band: b, level: l } => {
                prop_assert_eq!(b, band);
                prop_assert_eq!(l, level);
            }
            other => prop_assert!(false, "wrong: {other:?}"),
        }
    }

    // TN (TNC mode) is a bare read command — no write variant, so no round-trip.
    // CTCSS tone is configured through the FO (full channel) command.

    // 15. ME wire round-trip: serialize SetMemoryChannel, parse as MemoryChannel
    #[test]
    fn me_wire_round_trip(ch_num in 0u16..1000, channel in arb_channel_memory()) {
        let cmd = Command::SetMemoryChannel { channel: ch_num, data: channel.clone() };
        let wire = protocol::serialize(&cmd);
        let frame = &wire[..wire.len() - 1]; // strip \r
        let response = protocol::parse(frame).unwrap();
        match response {
            Response::MemoryChannel { channel: num, data: parsed } => {
                prop_assert_eq!(num, ch_num);
                prop_assert_eq!(parsed, channel);
            }
            other => prop_assert!(false, "wrong variant: {other:?}"),
        }
    }
}
