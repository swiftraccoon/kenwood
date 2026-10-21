//! Exercise slow-data assembly and the reversible three-byte scrambler.
#![no_main]

use dstar_gateway_core::{SlowDataAssembler, descramble, scramble};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut asm = SlowDataAssembler::default();
    for (index, chunk) in data.chunks_exact(3).enumerate() {
        let [first, second, third] = chunk else {
            unreachable!("chunks_exact yields complete three-byte chunks");
        };
        let arr = [*first, *second, *third];
        assert_eq!(
            descramble(scramble(arr)),
            arr,
            "scrambling is reversible for every payload"
        );
        let frame_index = u8::try_from(index % 21).unwrap_or(0);
        let _assembled = asm.push(arr, frame_index);
    }
});
