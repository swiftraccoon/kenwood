//! Mock radio scenarios for integration tests.
//!
//! This module is gated behind the `testing` cargo feature; it is not
//! compiled into release builds. Each scenario is a pre-scripted
//! sequence of `(request_bytes, response_bytes)` pairs that drive a
//! [`MockTransport`] through a known exchange.
//!
//! Used by the `--mock-radio <name>` CLI flag, which is also gated
//! behind the same `testing` feature.
//!
//! ## Coverage
//!
//! The current scenarios are intentionally minimal — just enough to
//! let an integration test exit the REPL loop cleanly after reading
//! the radio model. More elaborate scenarios (D-STAR, APRS) belong
//! in dedicated test fixture files loaded via
//! [`MockTransport::from_fixture`] rather than code-generated here.

use kenwood_thd75::transport::MockTransport;

/// Build a named mock scenario.
///
/// Returns `None` if `name` does not match a known scenario. Known
/// names: `"simple"` (minimal CAT identification flow), `"empty"`
/// (empty mock that rejects every write).
#[must_use]
pub fn build(name: &str) -> Option<MockTransport> {
    match name {
        "simple" => Some(simple_scenario()),
        "empty" => Some(MockTransport::new()),
        _ => None,
    }
}

/// Minimal scenario covering `Radio::connect_safe` plus one `identify`
/// round-trip and one `get_firmware_version` round-trip.
///
/// `connect_safe` sends a five-byte preamble (`\r`, `\r`, `ETX`,
/// `\rTC 1\r`, `TN 0,0\r`) followed by a drained read; each of those
/// writes is programmed against an empty response so the mock's
/// exchange queue drains cleanly. The subsequent CAT reads match the
/// `ID\r` and `FV\r` commands issued by the REPL startup.
fn simple_scenario() -> MockTransport {
    let mut mock = MockTransport::new();

    // connect_safe preamble. Each write is expected in order and
    // responds with empty bytes so the drain read at the end of
    // connect_safe is fed from the last pending_response slot.
    mock.expect(b"\r", b"");
    mock.expect(b"\r", b"");
    mock.expect(&[0x03], b"");
    mock.expect(b"\rTC 1\r", b"");
    mock.expect(b"TN 0,0\r", b"");

    // CAT identification round-trips. The REPL startup calls
    // radio.identify() then radio.get_firmware_version().
    mock.expect(b"ID\r", b"ID TH-D75\r");
    mock.expect(b"FV\r", b"FV 1.03.00\r");

    // From here on, accept any further writes without validation so
    // the integration test can run additional commands (`id`, `quit`)
    // without having to predict the exact wire output. Subsequent
    // reads will error (pending_response empty), which surfaces as
    // command-level errors that the script can absorb.
    mock.expect_any_write();

    mock
}
