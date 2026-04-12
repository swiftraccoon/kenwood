//! Golden wire-vector tests for `DCS` encoders.
//!
//! Each .bin file is the byte-exact output of a real `encode_*` call
//! against a known input. The .bin.txt sibling files document the
//! per-byte meaning.

// Integration tests are separate compilation units — each one must
// silence `unused_crate_dependencies` for workspace crates it doesn't
// `src/lib.rs`.
use proptest as _;
use static_assertions as _;
use thiserror as _;
use tracing as _;
use trybuild as _;

use dstar_gateway_core::codec::dcs::{
    encode_connect_ack, encode_connect_nak, encode_connect_unlink, encode_poll_request,
};
use dstar_gateway_core::types::{Callsign, Module};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn cs(s: &str) -> Result<Callsign, Box<dyn std::error::Error>> {
    Ok(Callsign::try_from_str(s)?)
}

fn m(c: char) -> Result<Module, Box<dyn std::error::Error>> {
    Ok(Module::try_from_char(c)?)
}

#[test]
fn golden_connect_unlink() -> TestResult {
    let expected = include_bytes!("golden/dcs/connect_unlink.bin");
    let mut buf = [0u8; 32];
    let n = encode_connect_unlink(&mut buf, &cs("W1AW")?, m('B')?, &cs("DCS001")?)?;
    assert_eq!(buf.get(..n).ok_or("n out of range")?, &expected[..]);
    Ok(())
}

#[test]
fn golden_connect_ack() -> TestResult {
    let expected = include_bytes!("golden/dcs/connect_ack.bin");
    let mut buf = [0u8; 32];
    let n = encode_connect_ack(&mut buf, &cs("DCS001")?, m('C')?)?;
    assert_eq!(buf.get(..n).ok_or("n out of range")?, &expected[..]);
    Ok(())
}

#[test]
fn golden_connect_nak() -> TestResult {
    let expected = include_bytes!("golden/dcs/connect_nak.bin");
    let mut buf = [0u8; 32];
    let n = encode_connect_nak(&mut buf, &cs("DCS001")?, m('C')?)?;
    assert_eq!(buf.get(..n).ok_or("n out of range")?, &expected[..]);
    Ok(())
}

#[test]
fn golden_poll_request() -> TestResult {
    let expected = include_bytes!("golden/dcs/poll_request.bin");
    let mut buf = [0u8; 32];
    let n = encode_poll_request(&mut buf, &cs("W1AW")?, &cs("DCS001")?)?;
    assert_eq!(buf.get(..n).ok_or("n out of range")?, &expected[..]);
    Ok(())
}
