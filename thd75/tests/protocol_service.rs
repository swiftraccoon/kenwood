//! Quarantine tests for unverified factory-service mnemonics.
//!
//! These operations are intentionally excluded from the shared CAT codec.
//! No public API may emit them until the selector-3 entry and exit sequence,
//! transport framing, and read/write semantics are independently live-verified.

use kenwood_thd75::error::ProtocolError;
use kenwood_thd75::protocol;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
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

#[test]
fn factory_service_responses_are_quarantined() {
    let frames: &[(&[u8], &str)] = &[
        (b"0E", "0E"),
        (b"0G", "0G"),
        (b"9R 000000,AF", "9R"),
        (b"9E 000000,AA", "9E"),
        (b"2V A1B2", "2V"),
    ];

    for &(frame, expected_mnemonic) in frames {
        let result = protocol::parse(frame);
        assert!(
            matches!(
                result,
                Err(ProtocolError::UnknownCommand(ref command))
                    if command == expected_mnemonic
            ),
            "{expected_mnemonic} unexpectedly remained in the shared CAT codec: {result:?}"
        );
    }
}
