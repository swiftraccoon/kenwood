# mbelib-rs

AMBE 3600×2400 voice codec for D-STAR. Pure Rust, no `unsafe`, no FFI.
Decoder on by default; encoder behind the `encoder` Cargo feature.

## Scope

- **Decoder** (default): 9-byte AMBE 3600×2400 voice frames → 160-sample
  PCM at 8 kHz (20 ms/frame, 50 frames/s).
- **Encoder** (`--features encoder`): 160-sample PCM → 9-byte AMBE frames.
  All 9 parameter fields (pitch / V/UV / gain / PRBA × 2 / HOC × 4) are
  populated from real signal analysis, FEC-encoded, scrambled, and packed
  to wire form. Chip-interop (DVSI hardware receiving our bytes) is
  untested as of this writing — use `--features encoder` at your own risk.
- **D-STAR only**: AMBE 3600×2400 is the mandatory voice codec for the
  JARL D-STAR standard. Does **not** support AMBE+, AMBE+2, IMBE, tone
  synthesis, or the AMBE+2 "half-rate" variant used by DMR / YSF / NXDN
  (which remains covered by US Patent 8,359,197 until 2028-05-20).

## Usage

### Decoder (always available)

```rust
use mbelib_rs::AmbeDecoder;

let mut decoder = AmbeDecoder::new();
let ambe_frame: [u8; 9] = [0; 9];
let pcm: [i16; 160] = decoder.decode_frame(&ambe_frame);
```

### Encoder (feature-gated)

```rust,ignore
use mbelib_rs::AmbeEncoder;

let mut encoder = AmbeEncoder::new();
let pcm: [f32; 160] = [0.0; 160]; // 20 ms @ 8 kHz, nominal [-1.0, 1.0)
let ambe: [u8; 9] = encoder.encode_frame(&pcm);
// Or from i16 PCM directly:
let pcm_i16: [i16; 160] = [0; 160];
let ambe: [u8; 9] = encoder.encode_frame_i16(&pcm_i16);
```

One decoder/encoder per voice stream (each carries inter-frame state).

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

## Encoder pipeline (feature-gated)

Symmetrical to decode, plus an analysis front-end:

1. DC-removal high-pass + 21-tap pitch-estimation LPF.
2. 221-sample windowed 256-point FFT for spectral analysis.
3. Sub-harmonic-summation pitch estimator (OP25 `pitch_est.cc` E(p)
   approach): for each candidate period `p`, score the normalized
   sum of windowed autocorrelations at `p`, `2p`, `3p`, …. The true
   fundamental accumulates contributions from every harmonic; sub-
   multiples miss half their terms. A soft log-distance-from-prior
   penalty breaks residual ties smoothly without hard-pinning.
4. Per-harmonic-band voiced/unvoiced decisions from harmonic-to-total
   energy ratios.
5. Per-harmonic magnitudes from 3-bin-integrated FFT power.
6. Parameter quantization against the mbelib codebooks (same tables as
   decode): L-constrained `W0_TABLE` search for pitch (pick the `b0`
   where `L_TABLE[b0] == num_harms` AND W0 closest to target),
   `VUV_TABLE` for voicing summary, `DG_TABLE` for gain, `PRBA24_TABLE`
   + `PRBA58_TABLE` for spectral envelope (via inverse 8-point DCT on
   Ri from block-DCT Cik), and `HOC_B5..B8_TABLE` for within-block
   detail. `b8` uses stride-2 search because the wire format forces
   its LSB to zero (mbelib decoder convention).
7. Closed-loop `prev_log2_ml` reconstruction: after emitting the
   49-bit parameter vector we run `decode_params()` internally to
   capture what the on-air decoder will reconstruct, and carry THAT
   state into the next frame's prediction residual computation —
   so encoder and decoder track identical magnitude history.
8. Golay(23,12) encode on C0 and C1 + outer parity.
9. LFSR scramble of C1 seeded from C0 data bits.
10. 72-bit interleave to transmission order, pack to 9 wire bytes.

### Validation infrastructure

Two `cargo run --release --example` harnesses validate against
OP25's reference implementation with identical inputs:

- `validate_quantize_vs_op25 <op25.trace>` — feeds OP25's exact
  `imbe_param` (sa, v_uv_dsn, ref_pitch, prev state) into our
  `quantize()` and checks `b[0..8]` byte-for-byte. As of the last
  pass: **b3/b4/b5/b6/b7 (spectral) 100%**, b2 (gain) 99%, b1 (VUV)
  88%, b0 (pitch) 60%, b8 (HOC_B8) 30% (OP25 off-spec in D-STAR
  mode — our stride-2 is correct per mbelib).
- `validate_analysis_vs_op25 <pcm> <op25.trace>` — runs our full
  analysis pipeline on identical PCM and compares pitch/num_harms
  against OP25's IMBE. ±5-sample pitch match ≈ 45% — the multi-
  frame DP look-ahead from OP25 `pitch_est.cc:229-281` is the
  remaining piece to close this gap.

Produce the `.trace` input with the `ambe_encode_dump` harness
built against an OP25 checkout (CLI: `ambe_encode_dump <pcm>
<out.ambe> <trace>`).

Derived from Max H. Parke (KA1RBI)'s `ambe_encoder.cc` and Pavel
Yazev's `imbe_vocoder` in [boatbod/op25](https://github.com/boatbod/op25)
(GPL-3.0-or-later, 2009–2016). Our Rust formulation simplifies the
ETSI fixed-point arithmetic to native f32 throughout; intelligibility
should match but bit-exact DVSI chip output is not guaranteed.

## Dependencies

- `realfft` — 256-point real FFT for unvoiced synthesis (decoder) and
  spectral analysis (encoder).
- `wide` — `f32x4`/`i32x4` SIMD for float→i16 conversion.

Both are pure Rust (MIT/Apache-2.0), so no C, no FFI, no `unsafe`.

## License

GPL-2.0-or-later (decoder) / GPL-3.0-or-later (encoder, feature-gated).

- Base decoder: ported from szechyjs's ISC-licensed
  [mbelib](https://github.com/szechyjs/mbelib).
- JMBE-parity decoder algorithms (FFT unvoiced, adaptive smoothing,
  phase interpolation): from arancormonk's GPL-2.0-or-later
  [mbelib-neo](https://github.com/arancormonk/mbelib-neo).
- D-STAR interleave tables: szechyjs's ISC-licensed
  [dsd](https://github.com/szechyjs/dsd).
- Encoder analysis path: ported from Max H. Parke (KA1RBI)'s
  `ambe_encoder.cc` + Pavel Yazev's `imbe_vocoder` in
  [boatbod/op25](https://github.com/boatbod/op25), GPL-3.0-or-later.
  Enabling the `encoder` feature activates GPL-3.0-or-later provisions
  for the resulting binary.

See [`LICENSE`](./LICENSE) for upstream copyright notices. Full license
texts in [`LICENSES/`](./LICENSES/).

## Patents

US patents commonly cited as covering AMBE 3600×2400 (all expired per
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
to AMBE 3600×2400 is debatable.

AMBE+, AMBE+2, and AMBE-3000 may be covered by additional patents and
are not implemented here.

This is not legal advice. Patent status can change (reexamination, term
adjustment, foreign equivalents); verify before commercial use.

Amateur radio transmissions are "readily accessible to the general
public" under 18 USC 2511(2)(g)(ii) in the US; consult local rules for
other jurisdictions.
