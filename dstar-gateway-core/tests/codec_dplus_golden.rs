//! Golden wire-vector tests for `DPlus` encoders.
//!
//! Each .bin file is the byte-exact output of a real `encode_*` call
//! against a known input. The .bin.txt sibling files document the
//! per-byte meaning.

use dstar_gateway_core::Callsign;
use dstar_gateway_core::codec::dplus::{
    Link2Result, encode_link1, encode_link2, encode_link2_reply, encode_poll, encode_unlink,
    parse_auth_response,
};
use dstar_gateway_core::validator::NullSink;

// Integration tests are separate compilation units — each one must
// silence `unused_crate_dependencies` for workspace crates it doesn't
// `src/lib.rs`.
use proptest as _;
use static_assertions as _;
use thiserror as _;
use tracing as _;
use trybuild as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn golden_link1() -> TestResult {
    let expected = include_bytes!("golden/dplus/link1.bin");
    let mut buf = [0u8; 16];
    let n = encode_link1(&mut buf)?;
    assert_eq!(buf.get(..n).ok_or("n out of range")?, &expected[..]);
    Ok(())
}

#[test]
fn golden_unlink() -> TestResult {
    let expected = include_bytes!("golden/dplus/unlink.bin");
    let mut buf = [0u8; 16];
    let n = encode_unlink(&mut buf)?;
    assert_eq!(buf.get(..n).ok_or("n out of range")?, &expected[..]);
    Ok(())
}

#[test]
fn golden_poll() -> TestResult {
    let expected = include_bytes!("golden/dplus/poll.bin");
    let mut buf = [0u8; 16];
    let n = encode_poll(&mut buf)?;
    assert_eq!(buf.get(..n).ok_or("n out of range")?, &expected[..]);
    Ok(())
}

#[test]
fn golden_link2_w1aw() -> TestResult {
    let expected = include_bytes!("golden/dplus/link2_w1aw.bin");
    let cs = Callsign::from_wire_bytes(*b"W1AW    ");
    let mut buf = [0u8; 32];
    let n = encode_link2(&mut buf, &cs)?;
    assert_eq!(buf.get(..n).ok_or("n out of range")?, &expected[..]);
    Ok(())
}

#[test]
fn golden_link2_okrw_reply() -> TestResult {
    let expected = include_bytes!("golden/dplus/link2_okrw_reply.bin");
    let mut buf = [0u8; 16];
    let n = encode_link2_reply(&mut buf, Link2Result::Accept)?;
    assert_eq!(buf.get(..n).ok_or("n out of range")?, &expected[..]);
    Ok(())
}

#[test]
fn golden_link2_busy_reply() -> TestResult {
    let expected = include_bytes!("golden/dplus/link2_busy_reply.bin");
    let mut buf = [0u8; 16];
    let n = encode_link2_reply(&mut buf, Link2Result::Busy)?;
    assert_eq!(buf.get(..n).ok_or("n out of range")?, &expected[..]);
    Ok(())
}

#[test]
fn golden_auth_response_three_hosts() -> TestResult {
    // A single DPlus auth chunk carrying three host records:
    //   REF001 @ 10.0.0.1
    //   REF030 @ 192.168.1.100
    //   XLX307 @ 203.0.113.45
    //
    // The XRF-prefix filter (see DPlusAuthenticator.cpp:151-192) skips
    // only callsigns starting with "XRF", not "XLX", so all three
    // records are accepted.
    let expected = include_bytes!("golden/dplus/auth_response_3hosts.bin");
    let mut sink = NullSink;
    let list = parse_auth_response(&expected[..], &mut sink)?;
    assert_eq!(list.len(), 3, "REF001 + REF030 + XLX307 all accepted");
    assert!(list.find("REF001").is_some());
    assert!(list.find("REF030").is_some());
    assert!(list.find("XLX307").is_some());
    Ok(())
}
