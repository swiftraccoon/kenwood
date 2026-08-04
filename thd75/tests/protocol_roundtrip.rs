//! Property-based round-trip tests for the CAT protocol.
//!
//! Uses `proptest` to verify that serialize-then-parse produces the
//! original values for genuine CAT commands and shared wire types.

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use ::aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use mmdvm as _;
use mmdvm_core as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::tone::{DcsCode, ToneCode};
use kenwood_thd75::types::*;
use kiss_tnc::{KissCommand, KissFrame, KissPort, decode_kiss_frame, encode_kiss_frame};

/// Convert any debug-printable error into a `TestCaseError` so `?` can be used
/// in proptest blocks without violating workspace `unwrap_used` policy.
fn to_test_err<E: std::fmt::Debug>(e: E) -> TestCaseError {
    TestCaseError::fail(format!("{e:?}"))
}

// ============================================================================
// Arbitrary strategies
// ============================================================================

fn arb_band() -> impl Strategy<Value = Band> {
    // Only A/B for protocol tests. Try_from will not fail: the range 0..2 is
    // within Band::COUNT. Converting the panic to an `Option::filter` keeps
    // proptest from panicking if someone widens the range and hits an invalid
    // value; `.prop_filter_map` would hide the bug but this unwrap is documented
    // to be unreachable.
    (0u8..2).prop_filter_map("invalid band", |i| Band::try_from(i).ok())
}

// ============================================================================
// Property-based tests
// ============================================================================

proptest! {
    // Frequency wire format round-trip
    #[test]
    fn frequency_wire_round_trip(hz in any::<u32>()) {
        let f = Frequency::new(hz);
        let wire = f.to_wire_string();
        let parsed = Frequency::from_wire_string(&wire).map_err(to_test_err)?;
        prop_assert_eq!(parsed.as_hz(), hz);
    }

    // Frequency LE bytes round-trip
    #[test]
    fn frequency_le_round_trip(hz in any::<u32>()) {
        let f = Frequency::new(hz);
        let parsed = Frequency::from_le_bytes(f.to_le_bytes());
        prop_assert_eq!(parsed.as_hz(), hz);
    }

    // ToneCode round-trip
    #[test]
    fn tone_code_round_trip(idx in 0u8..=ToneCode::MAX_INDEX) {
        let tc = ToneCode::new(idx).map_err(to_test_err)?;
        prop_assert_eq!(tc.as_raw(), idx);
    }

    // DcsCode round-trip
    #[test]
    fn dcs_code_round_trip(idx in 0u8..DcsCode::COUNT) {
        let dc = DcsCode::new(idx).map_err(to_test_err)?;
        prop_assert_eq!(dc.as_raw(), idx);
    }

    // AG (global AF gain) wire round-trip
    #[test]
    fn af_gain_round_trip(raw_level in 0u8..=AfGainLevel::MAX) {
        let level = AfGainLevel::new(raw_level).map_err(to_test_err)?;
        let wire = protocol::serialize(&Command::SetAfGain { level });
        let frame = wire
            .split_last()
            .map(|(_, rest)| rest)
            .ok_or_else(|| to_test_err("empty wire"))?;
        let response = protocol::parse(frame).map_err(to_test_err)?;
        let Response::AfGain { level: parsed } = response else {
            prop_assert!(false, "wrong response: {response:?}");
            return Ok(());
        };
        prop_assert_eq!(parsed, level);
    }

    // SQ (squelch) wire round-trip
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

    // MD (mode) wire round-trip
    #[test]
    fn md_round_trip(band in arb_band(), mode_val in 0u8..OperatingMode::COUNT) {
        let mode = OperatingMode::try_from(mode_val).map_err(to_test_err)?;
        let cmd = Command::SetOperatingMode { band, mode };
        let wire = protocol::serialize(&cmd);
        let frame = wire.split_last().map(|(_, rest)| rest).ok_or_else(|| to_test_err("empty wire"))?;
        let r = protocol::parse(frame).map_err(to_test_err)?;
        let Response::OperatingMode { band: b, mode: m } = r else {
            prop_assert!(false, "wrong: {r:?}");
            return Ok(());
        };
        prop_assert_eq!(b, band);
        prop_assert_eq!(m, mode);
    }

    // PC (power level) wire round-trip
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

    // KISS frame encode/decode round-trip.
    //
    // The TH-D75 only ever uses port 0; this exercises every defined
    // nibble-encoded KISS command with arbitrary payload data.
    #[test]
    fn kiss_frame_round_trip(
        command in (0u8..7).prop_filter_map("known command", KissCommand::from_byte),
        data in proptest::collection::vec(any::<u8>(), 0..100),
    ) {
        let frame = KissFrame { port: KissPort::TH_D75, command, data };
        let encoded = encode_kiss_frame(&frame);
        let decoded = decode_kiss_frame(&encoded).map_err(to_test_err)?;
        prop_assert_eq!(decoded, frame);
    }

    // TN (TNC mode) is a bare read command with no write variant, so no round-trip.
    // CTCSS tone is configured through the FO (full channel) command.

    // SH (filter width) wire round-trip
    #[test]
    fn sh_round_trip(mode in 0u8..FilterMode::COUNT, width in 0u8..5) {
        let filter_mode = FilterMode::try_from(mode).map_err(to_test_err)?;
        // AM mode max is 3, SSB/CW max is 4
        let max_width = if mode == 2 { 4 } else { 5 };
        if width < max_width {
            let filter_width = FilterWidthIndex::new(filter_mode, width).map_err(to_test_err)?;
            let cmd = Command::SetFilterWidth { width: filter_width };
            let wire = protocol::serialize(&cmd);
            let frame = wire.split_last().map(|(_, rest)| rest).ok_or_else(|| to_test_err("empty wire"))?;
            let r = protocol::parse(frame).map_err(to_test_err)?;
            let Response::FilterWidth { width: w } = r else {
                prop_assert!(false, "wrong: {r:?}");
                return Ok(());
            };
            prop_assert_eq!(w.mode(), filter_mode);
            prop_assert_eq!(w, filter_width);
        }
    }
}
