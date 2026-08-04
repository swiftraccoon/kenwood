#![no_main]

use dstar_gateway_core::SlowDataAssembler;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut asm = SlowDataAssembler::default();
    for (index, chunk) in data.chunks_exact(3).enumerate() {
        let arr = [chunk[0], chunk[1], chunk[2]];
        let frame_index = u8::try_from(index % 21).unwrap_or(0);
        let _ = asm.push(arr, frame_index);
    }
});
