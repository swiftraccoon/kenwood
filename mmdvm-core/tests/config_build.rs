//! Compile-smoke for the `ModemConfig` stub.
//!
//! The full `SetConfig` wire encoder isn't implemented yet — this
//! test exists only to ensure the public API surface compiles and
//! that the struct fields can be freely mutated.

use mmdvm_core::{ModemConfig, ModemMode};

// Dev-dep acknowledgement for the test crate.
use proptest as _;
use thiserror as _;
use tracing as _;

#[test]
fn idle_config_defaults() {
    let c = ModemConfig::idle();
    assert_eq!(c.mode, ModemMode::Idle);
    assert_eq!(c.tx_delay, 0);
    assert_eq!(c.mode_flags, 0);
}

#[test]
fn fields_are_writable() {
    let mut c = ModemConfig::idle();
    c.mode = ModemMode::DStar;
    c.mode_flags = 0x01;
    c.rx_level = 50;
    c.tx_level = 128;
    c.tx_delay = 10;
    c.invert = 0x02;
    assert_eq!(c.mode, ModemMode::DStar);
    assert_eq!(c.mode_flags, 0x01);
    assert_eq!(c.rx_level, 50);
    assert_eq!(c.tx_level, 128);
    assert_eq!(c.tx_delay, 10);
    assert_eq!(c.invert, 0x02);
}

#[test]
fn copy_trait_works() {
    let c = ModemConfig::idle();
    let d = c;
    // Both should still be usable — Copy semantics.
    assert_eq!(c.mode, d.mode);
}
