//! Mock radio scenarios for integration tests.
//!
//! This module is gated behind the `testing` cargo feature; it is not
//! compiled into release builds. Each scenario is a pre-scripted
//! sequence of `(request_bytes, response_bytes)` pairs that drive a
//! [`MockTransport`](kenwood_thd75::transport::MockTransport) through
//! a known exchange.
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
//! [`MockTransport::from_fixture`](kenwood_thd75::transport::MockTransport::from_fixture)
//! rather than code-generated here.

use kenwood_thd75::transport::MockTransport;

/// Build a named mock scenario.
///
/// Returns `None` if `name` does not match a known scenario. Known
/// names: `"simple"` (minimal CAT identification flow), `"empty"`
/// (empty mock that rejects every write), `"mmdvm"` (radio in DV
/// Gateway / Reflector Terminal Mode: CAT identification fails and an
/// MMDVM probe answers, so the REPL takes the terminal-mode path).
#[must_use]
pub fn build(name: &str) -> Option<MockTransport> {
    match name {
        "simple" => Some(simple_scenario()),
        "empty" => Some(MockTransport::new()),
        "mmdvm" => Some(mmdvm_scenario()),
        "aprs" => Some(aprs_scenario()),
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

/// Radio that identifies normally, then accepts a KISS-mode entry so
/// the APRS transmit commands can be driven end-to-end.
///
/// Extends the [`simple_scenario`] startup (connect preamble + `ID` +
/// `FV`) with the `TN 2,0\r` KISS-entry echo that
/// [`AprsClient::start`](kenwood_thd75::AprsClient) waits for at the
/// default 1200 bps. After that, [`MockTransport::expect_any_write`]
/// absorbs every KISS frame the transmit commands emit (they are
/// write-only), so an integration test can exercise `position`,
/// `compressed`, `mice`, `object`, `status`, and `motion` without
/// predicting exact wire bytes — the per-format wire encodings are
/// already pinned by the `kenwood-thd75` and `aprs` unit tests.
fn aprs_scenario() -> MockTransport {
    let mut mock = MockTransport::new();

    // connect_safe preamble (identical to the simple scenario).
    mock.expect(b"\r", b"");
    mock.expect(b"\r", b"");
    mock.expect(&[0x03], b"");
    mock.expect(b"\rTC 1\r", b"");
    mock.expect(b"TN 0,0\r", b"");

    // Startup identification, so the REPL enters normal CAT mode.
    mock.expect(b"ID\r", b"ID TH-D75\r");
    mock.expect(b"FV\r", b"FV 1.03.00\r");

    // `aprs start` enters KISS mode at 1200 bps (TncBaud::Bps1200 = 0),
    // which sends `TN 2,0\r` and waits for the echo.
    mock.expect(b"TN 2,0\r", b"TN 2,0\r");

    // Every subsequent KISS transmit frame and the KISS-exit frame from
    // `aprs stop` are write-only; absorb them without validation.
    mock.expect_any_write();

    mock
}

/// Radio in a DV Gateway / Reflector Terminal Mode: CAT identification
/// fails (the radio speaks MMDVM binary, not CAT) but the MMDVM
/// `GET_VERSION` probe answers, so `Radio::diagnose_link` classifies
/// the link as [`kenwood_thd75::LinkDiagnosis::MmdvmMode`] and the REPL
/// takes the terminal-mode startup path.
///
/// Used by the integration test that asserts the terminal-mode guard
/// intercepts CAT commands with guidance instead of letting each one
/// block for the full command timeout.
fn mmdvm_scenario() -> MockTransport {
    let mut mock = MockTransport::new();

    // connect_safe preamble (same as the simple scenario) — each write
    // is programmed against an empty response so the drain read at the
    // end of connect_safe is satisfied cleanly.
    mock.expect(b"\r", b"");
    mock.expect(b"\r", b"");
    mock.expect(&[0x03], b"");
    mock.expect(b"\rTC 1\r", b"");
    mock.expect(b"TN 0,0\r", b"");

    // identify() sends `ID\r`. In a DV Gateway mode the CAT parser is
    // offline, so the radio answers `?` (or nothing); `?` keeps the
    // transport alive while making identify() return an error, which is
    // what drives the REPL into the diagnose_link path.
    mock.expect(b"ID\r", b"?\r");

    // diagnose_link() sends the MMDVM GET_VERSION frame and an
    // 0xE0-framed reply is positive proof of a DV Gateway mode.
    mock.expect(b"\xE0\x03\x00", b"\xE0\x0F\x00\x01MMDVM");

    // Absorb any trailing writes (the `quit` path calls disconnect()).
    // No CAT command in this scenario reaches the radio: the
    // terminal-mode guard intercepts them before dispatch.
    mock.expect_any_write();

    mock
}
