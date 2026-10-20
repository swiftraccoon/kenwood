#![no_main]
//! Checks byte-preserving serialization of accepted MCP configuration files.

use kenwood_tmd750::file::RadioConfig;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(config) = RadioConfig::from_bytes(data) {
        assert_eq!(
            config.to_bytes(),
            data,
            "a parsed file serializes to its bytes"
        );
    }
});
