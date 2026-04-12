//! Compile-fail tests proving tokio-shell typestate invariants.
//!
//! Each file under `tests/compile_fail/` is a standalone Rust program
//! that **must** fail to compile. `trybuild` compares the captured
//! rustc output against a sibling `.stderr` file. The suite fails if
//! any file compiles successfully (the typestate has regressed).
//!
//! To regenerate `.stderr` snapshots after an intentional diagnostic
//! change, run:
//!
//! ```bash
//! TRYBUILD=overwrite cargo test -p dstar-gateway --test trybuild_compile_fail
//! ```

use dstar_gateway as _;
use dstar_gateway_core as _;
use pcap_parser as _;
use thiserror as _;
use tokio as _;
use tracing as _;
use tracing_subscriber as _;

#[test]
fn compile_fail_shell() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
