#![no_main]

use kenwood_tmd750::file::parse_d750;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(config) = parse_d750(data) {
        assert_eq!(
            config.to_bytes(),
            data,
            "a parsed file serializes to its bytes"
        );
    }
});
