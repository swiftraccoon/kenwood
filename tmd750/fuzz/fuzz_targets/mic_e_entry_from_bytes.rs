#![no_main]

use kenwood_tmd750::memory::mic_e::{MIC_E_ENTRY_SIZE, MicETypeCode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(bytes) = <[u8; MIC_E_ENTRY_SIZE]>::try_from(data)
        && let Ok(Some(entry)) = MicETypeCode::from_bytes(bytes)
    {
        assert_eq!(entry.to_bytes(), bytes, "a decoded entry keeps every byte");
        assert_eq!(
            MicETypeCode::new(entry.code(), entry.model()),
            Ok(entry),
            "a decoded entry is accepted back unchanged"
        );
    }
});
