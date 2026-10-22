# mbelib-rs

Pure Rust AMBE 3600x2400 voice codec for D-STAR.

## Licensing and provenance

- Decode follows szechyjs/mbelib and arancormonk/mbelib-neo (GPL-2.0-or-later through the mbelib relicensing pathway). Encode ports Max Parke's OP25 `ambe_encoder.cc` and Pavel Yazev's `imbe_vocoder/v_uv_det.cc`.
- The `kenwood-tables` filter coefficients and postfilter weights are lifted from TH-D75 firmware under DMCA 1201(f); their doc comments deliberately carry firmware provenance, which is a reviewed standing exception to the root policy. Do not strip it.
- The implemented wire format is AMBE 3600x2400. Keep AMBE+2 half-rate, IMBE and other variants outside this codec.

## Commands

```bash
cargo nextest run -p mbelib-rs --features encoder --all-targets
cargo nextest run -p mbelib-rs --features kenwood-tables --all-targets   # implies encoder
cargo nextest run -p mbelib-rs --features wave-enhance --all-targets     # parity suites
cargo run -p mbelib-rs --example validate_bvec_vs_op25 --features encoder -- <pcm> <op25-trace>
cargo test -p mbelib-rs --features kenwood-tables --test kenwood_hitl_anchors -- --ignored --nocapture rust_encoder_field_breakdown
```

- Only `golden`, `roundtrip`, and `tuning` always compile. Every other test target and every `validate_*_vs_op25` example is declared with `required-features`, so a bare `cargo test` skips it silently.
- The `wave-enhance` suites reach lint.sh only through the workspace test pass, because `sextant` enables that feature and Cargo unifies it. A scoped `-p mbelib-rs` run must name the feature itself.
- The repo root is a virtual workspace, so `-p mbelib-rs` is required on example runs.
- `decode_ambe_stream` reads checked `MBELIB_TUNING=alpha=..,exp=..,lo=..,hi=..,uv=..` values and applies the offline enhancer when `MBELIB_WAVE_ENHANCE=1`. With `kenwood-tables`, `MBELIB_KENWOOD_POSTFILTER=1` explicitly selects the Kenwood envelope algorithm. Feature unification alone keeps the reference algorithms.

## Quality gate

- Blind listening verdicts are the ONLY ship gate for a synthesis or enhancement change. The log-mel score at or below 3.4 kHz is a screening proxy: a metric win without a verdict majority does not ship.
- Decoder defaults stay at parity. `SynthesisTuning::PARITY == Default` is bit-exact with the untuned decoder, pinned by `tests/tuning.rs`; keep it that way.
- The `kenwood-tables` envelope postfilter scores slightly worse against a DVSI-grade reference because it shapes toward TH-D75 speaker voicing. That is a different target, not a defect: do not "fix" it from that metric.
- Envelope-domain enhancement is perceptually closed: every magnitude and band-EQ candidate was judged inaudible while the full reference gap is consistently audible. Do not fund further envelope post-filters or re-sweep the tuning knobs without new evidence.
- Waveform-model weights ship as embedded blobs behind `wave-enhance`. A new checkpoint re-runs the exporter and must pass the parity test before shipping; the forward pass changes only when the architecture does.

## Wire bit order (ground truth)

- DVSI wire frames are LSB-first per byte (dsdcc `dstar.cpp:566`), handled in `unpack.rs` and `encode/pack.rs`. Validated against a live reflector capture with zero FEC corrections and 0.98 envelope correlation versus the mbelib C reference.
- Layout-bug diagnosis: decode a reflector capture through this crate and the mbelib C reference, compare 20 ms RMS envelope correlation, and histogram the FEC-corrected wire-bit positions. A fixed lattice of positions is a layout bug; a random spread is genuine RF error.
- Never apply the historical diagnostic XOR to voice bytes. `apply_diagnostic_xor()` in `kenwood::anchors` serves ignored capture probes and is not in the encode path.
- The capture-integrity test tier is `#[ignore]`d because its baseline captures were sliced under the old bit order; recapture on hardware before any anchor analysis.
- Preserve decoder disposition: repeat previous raw parameters when `errs2 > 3`; erasure and invalid-tone descriptors emit silence and reset history; valid tones use the oscillator; mute after more than three repeats. History resets restore PARITY coefficients and retain explicit envelope algorithm selection.

## Encoder status

- `encoder_roundtrip::sine_roundtrip_has_nonzero_correlation_with_input` is `#[ignore]`d at correlation about 0.04. It gates "DVSI accepts our AMBE": until it passes above 0.5, audio sent to a hardware radio will not play, so do not chase wire-format hypotheses for "no audio at radio" symptoms while it fails.
- Do NOT lower `D_STAR_GAIN_ADJUST` below the OP25 value 7.5 to chase that synthetic test; it produced garbled real-voice output. Kenwood anchor diagnostics suggesting 0.0 are a synthetic artifact, and real radio-to-radio audio is the only valid gate.
- Pure-sine input is degenerate for AMBE (near-zero magnitude above the fundamental collapses the gain mean), so large per-field deltas on synthetic input are not a quality measure. Real voice puts the gain field back in its active range.
- `PITCH_CANDIDATES` in `src/encode/pitch.rs` covers periods 21-122 samples, that is 65.6-381 Hz. Inputs above 381 Hz octave-fold (440 to 219, 550 to 276). Widening it touches every array sized by `PITCH_CANDIDATES`.
- The spectral envelope path (PRBA and HOC in `src/encode/quantize.rs`) is the known-broken area.
- Historical TH-D75 candidates and a 58-bit mask live in `src/encode/kenwood/anchors.rs`. Recapture with verified framing before using them as codec targets. `apply_diagnostic_xor` has no voice-wire-format interpretation.
