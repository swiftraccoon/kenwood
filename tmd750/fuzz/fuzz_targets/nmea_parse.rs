#![no_main]

use kenwood_tmd750::nmea::{LineReader, RawSentence, Sentence};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(line) = std::str::from_utf8(data)
        && let Ok(raw) = RawSentence::parse(line)
    {
        assert!(line.starts_with('$'), "a parsed sentence starts with $");
        assert!(
            Sentence::parse(line).is_ok() || matches!(raw.formatter(), "GGA" | "RMC"),
            "only a decoded formatter can fail after framing"
        );
    }
    let mut reader = LineReader::new();
    let split = data.len() / 2;
    let mut sentences = reader.push(data.get(..split).unwrap_or_default());
    sentences.extend(reader.push(data.get(split..).unwrap_or_default()));
    let mut whole = LineReader::new();
    assert_eq!(
        sentences,
        whole.push(data),
        "splitting the stream does not change the sentences"
    );
});
