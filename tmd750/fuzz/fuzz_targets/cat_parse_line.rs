#![no_main]
//! Parses arbitrary CAT reply bytes without requiring a live connection.

use kenwood_tmd750::protocol::cat::Response;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    drop(Response::from_bytes(data));
});
