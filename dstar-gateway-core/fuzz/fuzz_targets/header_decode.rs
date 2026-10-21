//! Preserve decoded header fields through canonical CRC encoding.
#![no_main]

use dstar_gateway_core::{DstarHeader, ENCODED_LEN};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() == ENCODED_LEN {
        let mut arr = [0u8; ENCODED_LEN];
        arr.copy_from_slice(data);
        let header = DstarHeader::decode(&arr);
        assert_eq!(
            DstarHeader::decode(&header.encode()),
            header,
            "canonical CRC encoding preserves all decoded header fields"
        );
    }
});
