#![no_main]

use dstar_gateway_core::SlowDataAssembler;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut asm = SlowDataAssembler::default();
    for chunk in data.chunks_exact(3) {
        let arr = [chunk[0], chunk[1], chunk[2]];
        let _ = asm.push(arr);
    }
});
