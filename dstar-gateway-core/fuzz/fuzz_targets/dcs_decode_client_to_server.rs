#![no_main]

use dstar_gateway_core::codec::dcs::decode_client_to_server;
use dstar_gateway_core::validator::NullSink;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut sink = NullSink;
    let _ = decode_client_to_server(data, &mut sink);
});
