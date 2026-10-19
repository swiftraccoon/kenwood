#![no_main]

use kenwood_tmd750::protocol::mcp::{HEADER_LEN, Header};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(bytes) = <[u8; HEADER_LEN]>::try_from(data)
        && let Ok(header) = Header::decode(&bytes)
    {
        assert_eq!(
            header.encode(),
            bytes,
            "a decoded header re-encodes exactly"
        );
    }
});
