#![no_main]

use kenwood_tmd750::types::{StoredChannel, StoredChannelFlag};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(channel) = StoredChannel::from_bytes(data) {
        assert_eq!(
            channel.to_bytes().as_slice(),
            data,
            "a decoded record re-encodes exactly"
        );
    }
    if let Ok(flag) = StoredChannelFlag::from_bytes(data) {
        assert_eq!(
            flag.to_bytes().as_slice(),
            data,
            "a decoded flag re-encodes exactly"
        );
    }
});
