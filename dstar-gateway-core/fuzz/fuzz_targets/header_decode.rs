#![no_main]

use dstar_gateway_core::{DStarHeader, ENCODED_LEN};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() == ENCODED_LEN {
        let mut arr = [0u8; ENCODED_LEN];
        arr.copy_from_slice(data);
        let _ = DStarHeader::decode(&arr);
    }
});
