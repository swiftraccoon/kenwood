#![no_main]

use kenwood_tmd750::memory::dtmf::{DTMF_FIELD_SIZE, DtmfMemoryEntry};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some((name, code)) = data.split_first_chunk::<DTMF_FIELD_SIZE>()
        && let Ok(code) = <[u8; DTMF_FIELD_SIZE]>::try_from(code)
        && let Ok(Some(entry)) = DtmfMemoryEntry::from_bytes(*name, code)
    {
        assert_eq!(
            [entry.name_bytes(), entry.code_bytes()],
            [name, &code],
            "a decoded entry keeps every byte"
        );
        assert_eq!(
            DtmfMemoryEntry::new(entry.name(), entry.code()),
            Ok(entry),
            "a decoded entry is accepted back unchanged"
        );
    }
});
