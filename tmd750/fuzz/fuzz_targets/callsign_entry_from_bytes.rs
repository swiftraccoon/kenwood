#![no_main]
//! Checks callsign records, memo text, and decoded callsign updates.

use kenwood_tmd750::memory::StoredText;
use kenwood_tmd750::memory::callsign_list::{CALLSIGN_ENTRY_SIZE, CallsignEntry};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(bytes) = <[u8; CALLSIGN_ENTRY_SIZE]>::try_from(data)
        && let Ok(Some(entry)) = CallsignEntry::from_bytes(bytes)
    {
        assert_eq!(entry.to_bytes(), bytes, "a decoded entry keeps every byte");
        let memo = entry.memo();
        assert_eq!(
            StoredText::new(&memo.to_string()).ok(),
            Some(memo),
            "a decoded memo re-encodes exactly"
        );
        let mut rebuilt = entry.clone();
        assert!(
            rebuilt.set_callsign(entry.callsign()).is_ok(),
            "a decoded callsign is accepted back"
        );
        assert_eq!(
            rebuilt, entry,
            "rewriting the decoded callsign changes nothing"
        );
    }
});
