//! Property-based round-trip tests for the CAT protocol.
//!
//! Uses `proptest` to verify that serialize-then-parse produces the
//! original values, and that binary packing round-trips exactly.

use proptest::prelude::*;

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::tone::{CtcssMode, DataSpeed, DcsCode, LockoutMode, ToneCode};
use kenwood_thd75::types::*;

// ============================================================================
// Arbitrary strategies
// ============================================================================

fn arb_band() -> impl Strategy<Value = Band> {
    (0u8..2).prop_map(|i| Band::try_from(i).unwrap()) // Only A/B for protocol tests
}

fn arb_channel_memory() -> impl Strategy<Value = ChannelMemory> {
    // Split into two sub-tuples to stay within proptest's 12-element limit.
    let part_a = (
        any::<u32>(),  // rx_frequency
        any::<u32>(),  // tx_offset
        (0u8..12),     // step_size
        (0u8..4),      // shift
        any::<bool>(), // reverse
        any::<bool>(), // tone_enable
        (0u8..3),      // ctcss_mode
        any::<bool>(), // dcs_enable
        any::<bool>(), // cross_tone_reverse
        (0u8..64),     // flags_0a_raw (6 bits)
    );
    let part_b = (
        (0u8..50),       // tone_code
        (0u8..50),       // ctcss_code
        (0u8..104),      // dcs_code
        (0u8..2),        // data_speed
        (0u8..3),        // lockout
        "[A-Z0-9]{0,8}", // urcall (alphanumeric only for wire safety)
        any::<u8>(),     // data_mode
    );
    (part_a, part_b).prop_map(
        |(
            (rx, tx, step, shift, rev, tone, ctcss_m, dcs, xrev, flags),
            (tc, cc, dc, ds, lo, urcall, dm),
        )| {
            ChannelMemory {
                rx_frequency: Frequency::new(rx),
                tx_offset: Frequency::new(tx),
                step_size: StepSize::try_from(step).unwrap(),
                shift: ShiftDirection::try_from(shift).unwrap(),
                reverse: rev,
                tone_enable: tone,
                ctcss_mode: CtcssMode::try_from(ctcss_m).unwrap(),
                dcs_enable: dcs,
                cross_tone_reverse: xrev,
                flags_0a_raw: flags,
                tone_code: ToneCode::new(tc).unwrap(),
                ctcss_code: ToneCode::new(cc).unwrap(),
                dcs_code: DcsCode::new(dc).unwrap(),
                data_speed: DataSpeed::try_from(ds).unwrap(),
                lockout: LockoutMode::try_from(lo).unwrap(),
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

    // 2. 40-byte binary round-trip
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
            shift: ShiftDirection::try_from(shift).unwrap(),
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        prop_assert_eq!(bytes[0x08] >> 4, step);
        prop_assert_eq!(bytes[0x08] & 0x0F, shift);
    }

    // 4. Byte 0x09 packing (reverse + tone + ctcss)
    #[test]
    fn byte_09_packing(rev in any::<bool>(), tone in any::<bool>(), ctcss in 0u8..3) {
        let ch = ChannelMemory {
            reverse: rev,
            tone_enable: tone,
            ctcss_mode: CtcssMode::try_from(ctcss).unwrap(),
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        prop_assert_eq!((bytes[0x09] >> 4) & 1, u8::from(rev));
        prop_assert_eq!((bytes[0x09] >> 2) & 1, u8::from(tone));
        prop_assert_eq!(bytes[0x09] & 0x03, ctcss);
    }

    // 5. Byte 0x0A packing (dcs + cross + flags)
    #[test]
    fn byte_0a_packing(dcs in any::<bool>(), cross in any::<bool>(), flags in 0u8..64) {
        let ch = ChannelMemory {
            dcs_enable: dcs,
            cross_tone_reverse: cross,
            flags_0a_raw: flags,
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        prop_assert_eq!((bytes[0x0A] >> 7) & 1, u8::from(dcs));
        prop_assert_eq!((bytes[0x0A] >> 6) & 1, u8::from(cross));
        prop_assert_eq!(bytes[0x0A] & 0x3F, flags);
    }

    // 6. Byte 0x0E packing (speed + lockout)
    #[test]
    fn byte_0e_packing(speed in 0u8..2, lockout in 0u8..3) {
        let ch = ChannelMemory {
            data_speed: DataSpeed::try_from(speed).unwrap(),
            lockout: LockoutMode::try_from(lockout).unwrap(),
            ..ChannelMemory::default()
        };
        let bytes = ch.to_bytes();
        prop_assert_eq!(bytes[0x0E] >> 4, speed);
        prop_assert_eq!(bytes[0x0E] & 0x03, lockout);
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
    fn sq_round_trip(band in arb_band(), level in 0u8..10) {
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
