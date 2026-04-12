//! Compile-fail test runner for the server crate.
//!
//! Each file in `tests/compile_fail/` is a standalone Rust program
//! that **must** fail to compile. `trybuild` compares the actual
//! `rustc` output against a `.stderr` file captured alongside the
//! source. The test suite fails if any file compiles successfully
//! (a typestate has regressed).
//!
//! This runner only covers the server crate's own public API —
//! specifically `ReflectorConfig`'s typed-builder required-field
//! enforcement. Server-side session typestate compile-fail tests
//! (state-gated `handle_voice_data` / `handle_link2` /
//! `handle_unlink`) live in `dstar-gateway-core/tests/compile_fail/`
//! because the types they exercise (`ServerSession<P, S>`,
//! state markers) are defined in `dstar-gateway-core`.

// Integration tests are separate compilation units. Acknowledge
// workspace deps that this runner does not reference directly so the
// strict `unused_crate_dependencies` lint stays quiet — only
// `trybuild` is actually touched below, everything else is needed
// by the compile_fail targets that trybuild compiles separately.
use dstar_gateway_core as _;
use dstar_gateway_server as _;
use proptest as _;
use thiserror as _;
use tokio as _;
use tracing as _;
use tracing_subscriber as _;

#[test]
fn compile_fail_reflector_typestate() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
