#![no_main]

use kenwood_tmd750::memory::repeater_list::{VOICE_CODE_SIZE, VoiceCode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(bytes) = <[u8; VOICE_CODE_SIZE]>::try_from(data)
        && let Ok(code) = VoiceCode::from_bytes(bytes)
    {
        assert_eq!(code.as_bytes(), &bytes, "a decoded code keeps every byte");
        let text = code.as_str();
        assert!(
            bytes.starts_with(text.as_bytes()),
            "the code is the stored prefix"
        );
        assert_eq!(
            VoiceCode::new(text).map(|code| code.as_str().to_owned()),
            Ok(text.to_owned()),
            "a decoded code is accepted back"
        );
    }
});
