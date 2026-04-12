//! Compile-fail tests proving typestate invariants.
//!
//! Each file in `tests/compile_fail/` is a standalone Rust program
//! that **must** fail to compile. `trybuild` compares the actual
//! rustc output against a `.stderr` file captured alongside each
//! `.rs` source. The test suite fails if any file compiles
//! successfully (the typestate has regressed).

// Integration tests are separate compilation units — each one must
// silence `unused_crate_dependencies` for workspace crates it doesn't
// directly use. Only `trybuild` is actually referenced from the
// runner, but the compile-fail test files under `tests/compile_fail/`
// DO use `dstar_gateway_core`; they're compiled as separate crates by
// trybuild, so from this runner's perspective the lib crate is
// "unused" and must be acknowledged here.
use dstar_gateway_core as _;
use proptest as _;
use static_assertions as _;
use thiserror as _;
use tracing as _;

#[test]
fn compile_fail_typestate() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
