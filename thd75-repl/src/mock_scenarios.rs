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
//! The normal CAT scenario is intentionally minimal. APRS and D-STAR
//! scenarios pair command-script fixtures with exact byte sequences here,
//! because their binary KISS and MMDVM frames cannot be represented by the
//! transport's text fixture format.

use kenwood_thd75::transport::MockTransport;

/// Build a named mock scenario.
///
/// Returns `None` if `name` does not match a known scenario. Known
/// names: `"simple"` (minimal CAT identification flow), `"empty"`
/// (empty mock that rejects every write), `"mmdvm"` (radio in DV
/// Gateway / Reflector Terminal Mode: CAT identification fails and an
/// MMDVM probe answers, so the REPL takes the terminal-mode path), and
/// `"mmdvm_dstar"` (the same startup followed by a strict D-STAR modem
/// initialization and noninteractive shutdown). `"mmdvm_dstar_idle"` allows
/// periodic status writes for an interactive prompt-liveness test.
#[must_use]
pub fn build(name: &str) -> Option<MockTransport> {
    match name {
        "simple" => Some(simple_scenario()),
        "empty" => Some(MockTransport::new()),
        "mmdvm" => Some(mmdvm_scenario()),
        "mmdvm_dstar" => Some(mmdvm_dstar_scenario()),
        "mmdvm_dstar_idle" => Some(mmdvm_dstar_idle_scenario()),
        "aprs" => Some(aprs_scenario()),
        _ => None,
    }
}

/// Minimal scenario covering `Radio::connect_with_tnc_exit` plus one `identify`
/// round-trip and one `get_firmware_version` round-trip.
///
/// `connect_with_tnc_exit` sends a six-write preamble (`\r`, `\r`, `ETX`, the
/// KISS Return frame `C0 FF C0`, `\rTC 1\r`, `TN 0,0\r`) followed by a
/// drained read; each of those writes is programmed against an empty
/// response so the mock's exchange queue drains cleanly. The subsequent
/// CAT reads match the `ID\r` and `FV\r` commands issued by the REPL
/// startup.
fn simple_scenario() -> MockTransport {
    let mut mock = MockTransport::new();

    // connect_with_tnc_exit preamble. Each write is expected in order and
    // responds with empty bytes so the drain read at the end of
    // connect_with_tnc_exit is fed from the last pending_response slot.
    mock.expect(b"\r", b"");
    mock.expect(b"\r", b"");
    mock.expect(&[0x03], b"");
    mock.expect(&[0xC0, 0xFF, 0xC0], b"");
    mock.expect(b"\rTC 1\r", b"");
    mock.expect(b"TN 0,0\r", b"");

    // CAT identification round-trips. The REPL startup calls
    // radio.identify() then radio.get_firmware_version().
    mock.expect(b"ID\r", b"ID TH-D75\r");
    mock.expect(b"FV\r", b"FV 1.03.00\r");

    // Scripted command round-trips (cat_basics.txt), in script order.
    mock.expect(b"BC\r", b"BC 0\r");
    mock.expect(b"IO\r", b"IO 0\r");

    // The final scripted `id` command performs another complete CAT
    // exchange. Keep the idle transport pending afterward, matching a
    // connected serial device with no unsolicited bytes.
    mock.expect(b"ID\r", b"ID TH-D75\r");
    mock.pend_when_empty();

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
/// write-only) while a later exact identity exchange proves CAT restoration,
/// so an integration test can exercise `position`,
/// `compressed`, `mice`, `object`, `status`, and `motion` without
/// predicting exact wire bytes; the per-format wire encodings are
/// already pinned by the `kenwood-thd75` and `aprs` unit tests.
fn aprs_scenario() -> MockTransport {
    let mut mock = MockTransport::new();

    // connect_with_tnc_exit preamble (identical to the simple scenario).
    mock.expect(b"\r", b"");
    mock.expect(b"\r", b"");
    mock.expect(&[0x03], b"");
    mock.expect(&[0xC0, 0xFF, 0xC0], b"");
    mock.expect(b"\rTC 1\r", b"");
    mock.expect(b"TN 0,0\r", b"");

    // Startup identification, so the REPL enters normal CAT mode.
    mock.expect(b"ID\r", b"ID TH-D75\r");
    mock.expect(b"FV\r", b"FV 1.03.00\r");

    // `aprs start` enters KISS mode at 1200 bps (PacketDataRate::Bps1200 = 0),
    // which sends `TN 2,0\r` and waits for the echo.
    mock.expect(b"TN 2,0\r", b"TN 2,0\r");

    // Every subsequent KISS transmit frame and the KISS-exit frame from
    // `aprs stop` are write-only; absorb them without validation. The exact
    // ID exchange remains queued until restore_cat_after_mode_exit sends it.
    mock.expect_any_write();
    mock.expect(b"ID\r", b"ID TH-D75\r");
    // CAT restoration proves a quiet boundary both before and after the
    // identity exchange. An idle serial link waits; it does not report a
    // read error merely because no byte is immediately available.
    mock.pend_when_empty();

    mock
}

/// Radio in a DV Gateway / Reflector Terminal Mode: CAT identification
/// fails (the radio speaks MMDVM binary, not CAT) but the MMDVM
/// `GET_VERSION` probe answers, so `Radio::probe_silent_link` classifies
/// the link as [`kenwood_thd75::LinkDiagnosis::MmdvmMode`] and the REPL
/// takes the terminal-mode startup path.
///
/// Used by the integration test that asserts the terminal-ready state
/// intercepts CAT commands with guidance instead of letting each one block for
/// the full command timeout.
fn mmdvm_scenario() -> MockTransport {
    let mut mock = MockTransport::new();

    // connect_with_tnc_exit preamble (same as the simple scenario): each write
    // is programmed against an empty response so the drain read at the
    // end of connect_with_tnc_exit is satisfied cleanly.
    mock.expect(b"\r", b"");
    mock.expect(b"\r", b"");
    mock.expect(&[0x03], b"");
    mock.expect(&[0xC0, 0xFF, 0xC0], b"");
    mock.expect(b"\rTC 1\r", b"");
    mock.expect(b"TN 0,0\r", b"");

    // identify() sends `ID\r`. In a DV Gateway mode the CAT parser is
    // offline, so the radio answers `?` (or nothing); `?` keeps the
    // transport alive while making identify() return an error, which is
    // what drives the REPL into the probe_silent_link path.
    mock.expect(b"ID\r", b"?\r");

    // probe_silent_link() sends the MMDVM GET_VERSION frame and an
    // 0xE0-framed reply is positive proof of a DV Gateway mode.
    mock.expect(b"\xE0\x03\x00", b"\xE0\x0E\x00\x01MMDVM 2018");

    mock
}

/// Radio already in Reflector Terminal Mode, followed by the exact MMDVM
/// writes needed to initialize a D-STAR gateway session.
///
/// This scenario deliberately has no catch-all write allowance. In
/// particular, a CAT recovery preamble sent after the positive binary-mode
/// proof fails immediately instead of being mistaken for valid gateway I/O.
fn mmdvm_dstar_scenario() -> MockTransport {
    let mut mock = mmdvm_dstar_init_scenario();

    // Script EOF stops the host gateway while preserving persistent Menu 650
    // mode. Shutdown must not emit the transient CAT-side `TN 0,0` command.
    mock.pend_when_empty();

    mock
}

/// Interactive D-STAR scenario that accepts periodic status polls while the
/// input prompt remains idle, then shuts the host gateway down at EOF without
/// a transient CAT-side exit.
fn mmdvm_dstar_idle_scenario() -> MockTransport {
    let mut mock = mmdvm_dstar_init_scenario();

    // The 250 ms MMDVM status poll is intentionally variable with wall-clock
    // scheduling. Accept those write-only frames while retaining the exact
    // shutdown boundary. The strict sibling scenario independently rejects
    // every unexpected startup/init write.
    mock.expect_any_write();
    mock.pend_when_empty();

    mock
}

/// Exact common startup through acknowledged D-STAR `SetMode`.
fn mmdvm_dstar_init_scenario() -> MockTransport {
    let mut mock = MockTransport::new();

    // connect_with_tnc_exit preamble.
    mock.expect(b"\r", b"");
    mock.expect(b"\r", b"");
    mock.expect(&[0x03], b"");
    mock.expect(&[0xC0, 0xFF, 0xC0], b"");
    mock.expect(b"\rTC 1\r", b"");
    mock.expect(b"TN 0,0\r", b"");

    // CAT is offline, but the complete MMDVM version frame proves the binary
    // protocol boundary and authorizes conversion into the typed session.
    mock.expect(b"ID\r", b"?\r");
    mock.expect(b"\xE0\x03\x00", b"\xE0\x0E\x00\x01MMDVM 2018");

    // AsyncModem's initial version and status requests precede gateway init.
    mock.expect(b"\xE0\x03\x00", b"");
    mock.expect(b"\xE0\x03\x01", b"");

    // D-STAR-only SetConfig, then SetMode(Dstar), each with its correlated
    // MMDVM acknowledgement.
    mock.expect(b"\xE0\x09\x02\x00\x01\x0A\x01\x80\x80", b"\xE0\x04\x70\x02");
    mock.expect(b"\xE0\x04\x03\x01", b"\xE0\x04\x70\x03");

    mock
}
