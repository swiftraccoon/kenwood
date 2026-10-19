#![no_main]

use kenwood_tmd750::protocol::cat::parse_line;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_line(data);
});
