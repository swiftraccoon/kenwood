#![no_main]
//! Checks repeater records and their stored-text round trips.

use kenwood_tmd750::memory::StoredText;
use kenwood_tmd750::memory::repeater_list::{
    REPEATER_RECORD_SIZE, REPEATER_TEXT_SIZE, RepeaterRecord,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(bytes) = <[u8; REPEATER_RECORD_SIZE]>::try_from(data)
        && let Ok(Some(record)) = RepeaterRecord::from_bytes(bytes)
    {
        assert_eq!(
            record.to_bytes(),
            bytes,
            "a decoded record keeps every byte"
        );
        let name = record.name();
        assert_eq!(
            StoredText::new(&name.to_string()).ok(),
            Some(name),
            "a decoded name re-encodes exactly"
        );
    }
    if let Ok(bytes) = <[u8; REPEATER_TEXT_SIZE]>::try_from(data)
        && let Ok(text) = StoredText::from_bytes(bytes)
    {
        assert_eq!(
            StoredText::new(&text.to_string()).ok(),
            Some(text),
            "decoded text re-encodes exactly"
        );
    }
});
