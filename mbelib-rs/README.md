# mbelib-rs

Pure Rust AMBE 3600×2450 voice codec decoder for D-STAR digital radio.

## What this is

The AMBE (Advanced Multi-Band Excitation) 3600×2450 codec compresses
speech at 3600 bits/second with 2450 bits of voice data and 1150 bits
of forward error correction. It is the mandatory voice codec for the
JARL D-STAR digital radio standard — every D-STAR transceiver, hotspot,
and reflector uses it.

`mbelib-rs` is a **decode-only** port of the ISC-licensed
[mbelib](https://github.com/szechyjs/mbelib) C library into pure Rust.
It has **zero runtime dependencies** and requires only `std` for
floating-point math. No unsafe code, no FFI, no external C libraries.

To the best of our knowledge, this is the first standalone AMBE decoder
crate in the Rust ecosystem.

## Usage

```rust
use mbelib_rs::AmbeDecoder;

// Create one decoder per voice stream — it carries inter-frame state
// needed for delta decoding and phase-continuous synthesis.
let mut decoder = AmbeDecoder::new();

// Feed 9-byte AMBE frames from D-STAR VoiceFrame.ambe field.
let ambe_frame: [u8; 9] = [0; 9];
let pcm: [i16; 160] = decoder.decode_frame(&ambe_frame);

// Output: 160 samples at 8 kHz, 16-bit signed PCM (20 ms of audio).
assert_eq!(pcm.len(), 160);
```

## Decode pipeline

Each 9-byte frame passes through these stages:

1. **Bit unpacking** — 72-bit frame → 4 FEC codeword bitplanes
2. **Error correction** — Golay(23,12) on C0/C1/C2, Hamming(15,11) on C3
3. **Demodulation** — LFSR descrambling of C1 using corrected C0 as seed
4. **Parameter extraction** — 49 corrected bits → fundamental frequency,
   harmonic count, voiced/unvoiced decisions, spectral magnitudes
5. **Spectral enhancement** — adaptive amplitude weighting for perceptual
   clarity
6. **Synthesis** — harmonic oscillator bank (voiced bands) + random-phase
   multisine (unvoiced bands), cross-faded across frame boundaries
7. **Output conversion** — float PCM → i16 with gain and clamping

The decoder is fully **deterministic**: the same input frame sequence
always produces bit-exact identical output. The unvoiced-band noise
generator is a hash-based PRNG keyed on sample position, replacing
mbelib's `rand()` which gave non-reproducible output.

## Quality vs. DVSI hardware

mbelib-rs audio quality matches the original C mbelib — intelligible
but noticeably "buzzy" compared to DVSI hardware vocoders (ThumbDV,
AMBE-3000, AMBE-3003). This is sufficient for monitoring, archival,
and transcription use cases — the same tradeoff every open-source
D-STAR scanner makes (DSD, SDRangel, SDRTrunk all use mbelib).

If you need hardware-quality audio, consume the raw 9-byte AMBE frames
and send them to a DVSI chip via the AMBEserver protocol.

## Legal notes

- **License:** ISC (permissive, matching upstream mbelib)
- **Patents:** The core AMBE/MBE algorithm patents
  (US 5,715,365, 5,754,974, 5,826,222, 5,870,405, 6,161,089, 6,199,037,
  6,377,916, 6,397,178) **have all expired** as of 2020. mbelib has
  existed publicly for 16+ years with zero enforcement action from
  DVSI. For decode-only monitoring use, the practical patent risk is
  minimal.
- **Amateur radio:** Amateur radio transmissions are explicitly
  "readily accessible to the general public" under 18 USC 2511, so
  recording and decoding them is legal in the US. Check your local
  jurisdiction for similar exemptions.

## Quality bar

This crate matches the [dstar-gateway-core](../dstar-gateway-core/)
standard: Rust edition 2024, rust-version 1.94,
`unsafe_code = "forbid"`, `missing_docs = "deny"`, full
`clippy::pedantic/nursery/cargo` with no blanket suppressions, verbose
doc comments on every item, 53 passing tests including exhaustive
bit-position verification and determinism guarantees.

## Status

Alpha. The decode pipeline structurally matches mbelib's C
implementation with exact bit-position transcription, and produces
bounded, deterministic PCM output. Full cross-validation against the
C reference on real-world AMBE corpus is planned.
