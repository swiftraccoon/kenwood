// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Adversarial-input properties for the APRS parse entry points.
//!
//! Every byte this crate parses arrives from the open airwaves or a
//! public APRS-IS feed; hostile and malformed input is the norm, not
//! an edge case. `parse_aprs_data_full` is the single dispatcher that
//! fans arbitrary bytes into ~15 sub-parsers (position, Mic-E,
//! compressed, weather, telemetry, message, object, item, status...),
//! several of which run float parsing and power arithmetic on
//! attacker-controlled digits.
//!
//! The property under test is total-ness: for ANY input the parser
//! returns `Ok` or `Err`, never panics and never hangs. A crash here
//! is a remote denial of service on any station running the stack.

use aprs::{parse_aprs_data, parse_aprs_data_full};
use proptest::prelude::{ProptestConfig, any, prop_assert, proptest};

// Sibling crates appear in the workspace dev-dep graph for this
// compilation unit; acknowledge them so `unused_crate_dependencies`
// stays quiet.
use ax25_codec as _;
use kiss_tnc as _;
use thiserror as _;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The full dispatcher must survive arbitrary information fields
    /// paired with arbitrary destination strings (the destination
    /// drives Mic-E latitude decoding, so the two interact).
    #[test]
    fn parse_aprs_data_full_never_panics(
        info in proptest::collection::vec(any::<u8>(), 0..512),
        dest in "\\PC{0,9}",
    ) {
        // The contract is total-ness: any classification is fine, a
        // panic or hang is not.
        let parsed = parse_aprs_data_full(&info, &dest);
        prop_assert!(parsed.is_ok() || parsed.is_err());
    }

    /// The Mic-E-free entry point must be equally total.
    #[test]
    fn parse_aprs_data_never_panics(
        info in proptest::collection::vec(any::<u8>(), 0..512),
    ) {
        let parsed = parse_aprs_data(&info);
        prop_assert!(parsed.is_ok() || parsed.is_err());
    }

    /// Structured fuzzing of the payloads most likely to reach deep
    /// numeric paths: a leading APRS data-type identifier followed by
    /// arbitrary bytes. Random bytes rarely produce a valid type
    /// byte, so without this the deep parsers are barely exercised.
    #[test]
    fn typed_payloads_never_panic(
        type_id in proptest::sample::select(vec![
            b'!', b'=', b'/', b'@', b':', b';', b')', b'>', b'T', b'_', b'*', b'$', b'<',
        ]),
        rest in proptest::collection::vec(any::<u8>(), 0..256),
        dest in "\\PC{0,9}",
    ) {
        let mut info = vec![type_id];
        info.extend_from_slice(&rest);
        let parsed = parse_aprs_data_full(&info, &dest);
        prop_assert!(parsed.is_ok() || parsed.is_err());
    }

    /// Position payloads made of digits, separators and hemisphere
    /// letters: the shape that reaches the float-parsing and
    /// range-validation code rather than bouncing off the format
    /// check.
    #[test]
    fn position_shaped_payloads_never_panic(
        body in "[0-9]{0,4}\\.?[0-9]{0,4}[NSns][/\\\\][0-9]{0,5}\\.?[0-9]{0,4}[EWew].{0,32}",
    ) {
        let mut info = vec![b'!'];
        info.extend_from_slice(body.as_bytes());
        let parsed = parse_aprs_data(&info);
        prop_assert!(parsed.is_ok() || parsed.is_err());
    }
}
