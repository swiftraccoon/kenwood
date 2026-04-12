#![no_main]

use dstar_gateway_core::codec::dplus::parse_auth_response;
use dstar_gateway_core::validator::NullSink;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut sink = NullSink;
    let _ = parse_auth_response(data, &mut sink);
});
