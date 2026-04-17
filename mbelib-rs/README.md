# mbelib-rs

AMBE 3600×2450 voice codec decoder for D-STAR. Pure Rust, no `unsafe`,
no FFI.

## Scope

- **Decodes** 9-byte AMBE 3600×2450 voice frames into 160-sample PCM at
  8 kHz (20 ms/frame, 50 frames/s).
- **D-STAR only**: AMBE 3600×2450 is the mandatory voice codec for the
  JARL D-STAR standard.
- Does **not** support: encoding, AMBE+, AMBE+2, IMBE, tone synthesis.

## Usage

```rust
use mbelib_rs::AmbeDecoder;

let mut decoder = AmbeDecoder::new();
let ambe_frame: [u8; 9] = [0; 9];
let pcm: [i16; 160] = decoder.decode_frame(&ambe_frame);
```

One decoder per voice stream (it carries inter-frame state).

## Pipeline

1. Unpack 72 bits into 4 FEC codewords (C0, C1, C2, C3).
2. Golay(23,12) error correction on C0 and C1. C2 (11 bits) and C3 (14
   bits) are unprotected data.
3. LFSR demodulation of C1 using corrected C0 as seed.
4. Parameter extraction into 49-bit vector → `w0`, `L`, voiced/unvoiced
   decisions, spectral magnitudes. Erasure (b0=120..=123) and tone
   (b0=126..=127) frames fall through to error concealment (reuse
   previous frame, increment repeat counter).
5. Spectral amplitude enhancement.
6. JMBE adaptive smoothing (algorithms #111-116).
7. Frame muting: if error rate exceeds 9.6% or repeat counter reaches
   3, emit comfort noise.
8. Synthesis: voiced bands via windowed cosine oscillator bank (with
   JMBE phase/amplitude interpolation for low harmonics with stable
   pitch); unvoiced bands via a single 256-point FFT + WOLA combine
   (JMBE algorithms #117-126).
9. Float → i16 with SIMD gain/clamp.

Deterministic: same input sequence produces bit-identical output.

## Dependencies

- `realfft` — 256-point real FFT for unvoiced synthesis.
- `wide` — `f32x4`/`i32x4` SIMD for float→i16 conversion.

Both are pure Rust (MIT/Apache-2.0), so no C, no FFI, no `unsafe`.

## License

GPL-2.0-or-later. The base decoder is ported from szechyjs's
ISC-licensed [mbelib](https://github.com/szechyjs/mbelib); JMBE-parity
algorithms (FFT unvoiced, adaptive smoothing, phase interpolation) are
ported from arancormonk's GPL-2.0-or-later
[mbelib-neo](https://github.com/arancormonk/mbelib-neo). D-STAR
interleave tables are from szechyjs's ISC-licensed
[dsd](https://github.com/szechyjs/dsd).

See [`LICENSE`](./LICENSE) for upstream copyright notices. Full license
texts in [`LICENSES/`](./LICENSES/).

## Patents

US patents commonly cited as covering AMBE 3600×2450 (all expired per
Google Patents):

| Patent          | Assignee               | Expired      |
|-----------------|------------------------|--------------|
| US 5,715,365    | Digital Voice Systems  | 2015-02-03   |
| US 5,754,974    | Digital Voice Systems  | 2015-05-19   |
| US 5,826,222    | Digital Voice Systems  | 2015-01-12   |
| US 5,870,405    | Digital Voice Systems  | 2012-11-30   |
| US 6,161,089    | Digital Voice Systems  | 2017-03-14   |
| US 6,199,037    | Digital Voice Systems  | 2017-12-04   |
| US 6,377,916    | Digital Voice Systems  | 2019-11-29   |
| US 6,397,178    | Conexant Systems       | 2018-09-18 † |

† US 6,397,178 is a generic speech-coding patent, included here because
it appears in commonly-circulated AMBE patent lists; its applicability
to AMBE 3600×2450 is debatable.

AMBE+, AMBE+2, and AMBE-3000 may be covered by additional patents and
are not implemented here.

This is not legal advice. Patent status can change (reexamination, term
adjustment, foreign equivalents); verify before commercial use.

Amateur radio transmissions are "readily accessible to the general
public" under 18 USC 2511(2)(g)(ii) in the US; consult local rules for
other jurisdictions.
