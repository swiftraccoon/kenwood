# mbelib-rs Deep Code Review

## Scope & scoring notes

The caller cited "28 unwrap/expect in quantize.rs" — those 28 are `#[expect(...)]` lint suppressions, not runtime `.unwrap()` calls. A `grep -rE '\.(unwrap|expect)\('` over `src/` finds **zero** runtime panic sites in the decoder or encoder; the module authors have systematically replaced indexing with `.get().unwrap_or(...)`. The `#[expect]` attributes on numeric casts are mostly well-justified — the loop bounds do keep the values in range. So decoder-panic risk is genuinely low. The findings below focus on the panics that remain, math edge cases, and documentation/algorithm drift.

---

## Decoder (untrusted RF input)

### `mbelib-rs/src/decode.rs`

- **`decode.rs:190` — `unvc = 0.2046 / cur.w0.sqrt()` skipped for silence but not fully guarded.** In the silence branch `decode_frequency` sets `cur.w0 = SILENCE_W0` (positive), and for non-silence b0 ∈ 0..=119 `W0_TABLE.get(b0)` is always positive. So no div-by-zero *for well-formed callers* — but the invariant "no non-erasure/non-tone frame reaches `unvc` computation with w0 == 0" is only implicit; a future refactor that widens the b0 range or changes the tone/erasure disposition could silently produce Inf/NaN that flows all the way to synthesis. Consider `let unvc = 0.2046 / cur.w0.sqrt().max(1e-6);`.

- **`decode.rs:258` — `let jl = (l as f32 * 16.0 * f0) as usize;` used without clamp before `vuv_row.and_then(|row| row.get(jl))`.** Safe today because VUV_TABLE rows are 8 wide and `.get(jl)` returns None for `jl >= 8`, defaulting to 0 → unvoiced. But `l` here iterates to `cur.l` (up to 56) and `f0` up to `W0_TABLE[0] ≈ 0.05`, so `jl` up to ~44 — the vast majority of bands beyond band 1 silently read "unvoiced". This matches mbelib but is worth pinning with a test.

- **`decode.rs:657–659` — `T5/T6/T7` tone-index tables have no citation.** Bit layout copied from `mbelib/ambe3600x2400.c:176-218`, but the three 8-entry arrays are transcribed magic numbers with no golden reference in the tree. A single bit-flip during transcription would silently misroute tone frames. Consider adding a comment giving the mbelib line numbers or a hex-dump comparison.

### `mbelib-rs/src/ecc.rs`

- **`ecc.rs:192–207` — error count returns *data-bit* corrections only, ignoring parity flips.** `golay_decode` counts only differences in bits 11..=22, so a codeword corrupted by three flips *all in the parity area* returns `errs = 0` and is treated as "fully trusted." Correspondingly, the top-level `errs2 > 3` gate in `lib.rs:613` never fires. This is faithful to mbelib, but under adversarial input a 3-parity-flip codeword receives `errs = 0` even though the syndrome is non-zero and the codeword was mis-received. Consider counting parity-position corrections too, at least for the gating decision.

- **`ecc.rs:222` — comment says the outer parity bit `ambe_fr[0]` is "noted as a TODO in mbelib" — but never validated here either.** So a single bit flip on wire bit 0 of C0 is silently accepted. Real DVSI chips *do* check it. Worth documenting the security tradeoff explicitly.

### `mbelib-rs/src/unpack.rs`

Clean. `INTERLEAVE` table verified as a permutation of 0..72 by unit test. LFSR uses `wrapping_*` correctly. LSB-first extraction confirmed against dsdcc + live capture.

### `mbelib-rs/src/adaptive.rs`

- **`adaptive.rs:161` — `AMPLITUDE_BASE - (AMPLITUDE_PENALTY_PER_ERROR * error_total) + prev_threshold` is signed i32 with no overflow check.** With `AMPLITUDE_PENALTY_PER_ERROR = 300` and prev_threshold accumulating over frames, sustained high error counts can push `tm` negative. Line 166–170 then casts negative `tm` to `f32`, and line 172 does `am > tm_f` — negative tm makes the condition always true → all magnitudes scaled by `tm_f / am` (a negative scalar), inverting sign of all bands. Effect: audible glitches, but no panic. Consider clamping `tm.max(MIN_TM)`.

- **`adaptive.rs:218` — `java_random_next_bits` shifts by `48 - bits`.** Callers always pass 24, so the shift is 24. But if a future caller passes `bits > 48`, the shift underflows: undefined behavior in Rust for the shift amount. Consider `debug_assert!(bits <= 48)`.

### `mbelib-rs/src/enhance.rs` & `mbelib-rs/src/synthesize.rs`

Clean; div-by-zero cases (Rm0² == Rm1², cur.l == 0) are guarded. Cross-fade extension in `extend_bands` (`synthesize.rs:180`) marks extended bands **voiced** — that's per JMBE, but the doc claim ("padded with zero magnitudes") on line 98–99 is misleading: only magnitudes are zero, voicing is set true. Doc drift.

### `mbelib-rs/src/unvoiced_fft.rs`

- **`unvoiced_fft.rs:255` — `let mut state = (params.noise_seed as u32) % LCG_M;` after the cold-start check.** After cold-start, `noise_seed = LCG_DEFAULT_SEED = 3147.0` (positive, small). But nothing prevents a caller from setting `params.noise_seed` to a huge positive or NaN. NaN cast to u32 is 0 in Rust (well-defined but silent). More concerning: a decoded frame with `noise_seed` between 0.0 and 53125.0 uses the seed directly, but if a *bug elsewhere* stores a value outside that range, the `.round()` in the JMBE spec is missing here — the raw truncation of `.as u32` differs from a proper `.round()`. Not directly exploitable, but a divergence from JMBE if the state ever leaves the LCG orbit.

- **`unvoiced_fft.rs:339–353` — `a_min`/`b_max` clamping.** `((l_f - 0.5) * mult).ceil()` can be negative for l=1 if w0 is very small (SILENCE_W0 = 2π/32 ≈ 0.196, mult = 256/2π × 0.196 = 8, a = ceil(0.5 × 8) = 4, positive). For very small w0 (which shouldn't happen), `l_f - 0.5 = 0.5` and mult tiny → a and b both ≈ 0, clamped to same → `continue` on line 353. No panic.

---

## Encoder (feature `encoder`, trusted local audio)

### `mbelib-rs/src/encode/quantize.rs`

- **`encode/quantize.rs:648` — `let band = ((l - 1) * vuv.num_bands / amps.num_harmonics).min(vuv.num_bands - 1);`.** `vuv.num_bands - 1` underflows (panics in debug) if `vuv.num_bands == 0`. The public `VuvDecisions` struct exposes this field directly, and no invariant enforces `>= 1`. The sibling `compute_lsa` on line 462 correctly uses `saturating_sub(1)`. Change to `vuv.num_bands.saturating_sub(1)` (or bail early if 0). The division `/ amps.num_harmonics` is guarded by the early return on line 622.

- **`encode/quantize.rs:1116–1132` — `nearest_hoc` with `.step_by(stride)`.** `stride == 0` panics `step_by`. `debug_assert!(stride == 1 || stride == 2)` catches this only in debug builds — a caller passing 0 in release triggers an assertion failure inside `step_by`. Callers today only pass literal 1 or 2, so it's fine in practice, but the guard is weaker than the doc implies.

- **`encode/quantize.rs:1023` — `ji_val.saturating_sub(2).min(4)` = `hoc_dims`.** For blocks with `Ji ≤ 2` this returns 0. Fine. For `Ji = 17`, returns 4. Fine.

- **`encode/quantize.rs:409` — closed-loop `decode_params` runs on encoder's own `ambe_d`.** This is correct — but note that if the encoder emits a b0 corresponding to erasure/tone (120..=127), `decode_params` returns early without touching `cur.log2_ml`, and the outcome carries stale zeros forward as `prev_log2_ml`. The pitch quantizer's silence guard (line 569, `confidence < 0.05`) emits b0 = 124, which lands in the silence branch of `decode_frequency` and *does* set `cur.l = 14` and populate `cur.log2_ml` — so this is actually OK. But if a future change ever emits b0 ∈ 120..=123 the prev state chain breaks silently.

### `mbelib-rs/src/encode/pitch_quant.rs`

- **`pitch_quant.rs:143, 176, 190, 202` — direct indexing `B0_LOOKUP[b0_i as usize]` with `#[expect(clippy::indexing_slicing)]`.** Bounds are guaranteed by the surrounding branches. But `debug_assert_eq!(B0_LOOKUP.len(), 827)` (line 113) can fire only in debug; in release, if `B0_LOOKUP` ever drifts to a different size, the walk overruns bounds silently. Change to `const _: () = assert!(B0_LOOKUP.len() == 827);` for a compile-time guarantee (`const_assert`-style).

### `mbelib-rs/src/encode/dc_rmv.rs`

- **`dc_rmv.rs:30–31` — doc claim "Panics if …" is false.** The function uses `sigin.iter().zip(sigout.iter_mut())` which iterates over the min length and cannot panic. Fix the doc.

### `mbelib-rs/src/encode/vuv.rs`

- **`encode/vuv.rs:213–221` — `bin_half = fft_out.len() / 2;` energy split.** For `fft_out.len() < 2`, `bin_half == 0` and both energies are 0 → later ratio-based `M_fcn` involves `0/0 = NaN`. Not a hazard for the real 256-pt FFT, but a defensive `.max(1)` on `bin_half` would eliminate the NaN class.

---

## Wave enhancers (feature `wave-enhance`)

### `mbelib-rs/src/enhance_wave/mod.rs` and `mbelib-rs/src/enhance_live/mod.rs`

- **`enhance_wave/mod.rs:159–160` and `enhance_live/mod.rs:205–206` — `conv2d` computes `(h + 2*ph - kh) / sh + 1`.** If `2*ph + h < kh`, the subtraction panics (debug) / wraps (release), then divides. All layer geometries baked in `read_conv` calls satisfy `2*ph + h >= kh` for `h = BINS = 129`, but the function accepts arbitrary `h`. Since it's only called with `BINS`, safe today.

- **Wave-enhance `enhance_wave::WaveEnhancer::process_f32`** — no short-clip guard (only `process` guards `pcm.len() < 512`). Callers dropping in a `< 512` sample slice via `process_f32` skip the passthrough, and the same call in `enhance_live::LiveWaveEnhancer::process_f32` also lacks the guard. Divergence between `process` and `process_f32` on short inputs — a caller expecting `.process_f32(&slice) == .process(&i16_slice)` for short slices gets different behavior.

- **`enhance_live` vs `enhance_wave` divergence.** By design: `enhance_live` uses a *forward-only* GRU (`GRU_H = 256`), `enhance_wave` uses bidirectional (`GRU_H = 128` per direction). Model weights are separate blobs. This is intended, but noted here because the caller specifically asked. The `reflect_pad`/`build_left` paths differ subtly — `enhance_wave::reflect_pad` uses `samples.get(i)` with `unwrap_or(0.0)`; `enhance_live::build_left` uses `staging.get(PAD - j)`. Both zero-fill short inputs, but the *value* zero-filled is different: enhance_wave pads with `samples[i]` for i=1..=128 (mirrors from index 1), enhance_live pads with `staging[PAD-j]` for j=0..PAD (mirrors from index 128 down to 1). For a full-length input these agree; for shorter-than-128 inputs the two produce different padded arrays. Unlikely to bite in production (the RELEASE_MIN=512 guard skips it) but worth pinning.

---

## Tables & general

- **`tables.rs:107` — `L_TABLE` last 6 entries are `56.0, 56.0, 56.0, 56.0, 56.0, 56.0 // last line is padded`.** Indices 120..=125 are erasure/silence. Padding with 56 (max valid L) rather than 0 is *safer* if a bug ever lets an erasure/silence code reach a synthesis path, but it hides the bug. Consider padding with a sentinel (e.g. `f32::NAN`) so misuses produce visible garbage and get caught. The `L_TABLE.get(b0).unwrap_or(&0.0)` in `decode.rs:233` maps the missing entries to 0.0 anyway, which is a different value from the in-table padding — subtle inconsistency between the two lookup paths (`decode_frequency` uses `.get()` → 0 for OOB, but for in-table indices 120..=125 gets 56).

- **`tables.rs:33` — module-scope `#[expect(clippy::unreadable_literal, clippy::excessive_precision, ...)]`.** Justified; codec tables must stay byte-identical to the C source.

- **`tables.rs` doc header says `AmbePlusLtable -> L_TABLE (126 entries, 7-bit b0 index)`** but the actual `W0_TABLE` and `L_TABLE` are declared `[f32; 126]` — matches. However, comment on `W0_TABLE` (line 50–53) says "Values for 120..=123 (erasure) are unused but filled with the silence value so out-of-range lookups return something sane instead of zero." — but the table starts at 4.998e-2 (b0=0) monotonically decreasing to 7.87e-3 (b0=125). No obvious constant-fill region; the doc claim is not what the table shows. Doc drift.

- **`ecc.rs:21–34` header table claims "AMBE 3600×2400 does not apply Hamming to C3; those 14 bits are copied verbatim into the parameter vector"** — matches `ecc_data`. Consistent.

- **`lib.rs:59` — "Adaptive smoothing — JMBE algorithms #111-116, gracefully damps spurious magnitudes/voicing decisions on noisy frames"** — matches adaptive.rs.

## Tests / TODOs

- No TODO/FIXME/HACK found in the source (`ecc.rs:222` mentions mbelib's TODO in a doc comment, not a live one).
- Tests are extensive: golay 3-flip sweep, decode determinism, tone/erasure/silence classification, encoder round-trip with concurrency in `AmbeParamExtractor`.

## Summary of highest-priority issues

1. **`encode/quantize.rs:648`** — `vuv.num_bands - 1` panics in debug when `num_bands == 0`; use `saturating_sub(1)`.
2. **`ecc.rs:192–207`** — Golay error counter ignores parity-bit corrections, so a codeword with 3 parity-only flips is silently accepted and the `errs2 > 3` gate never fires. Faithful to mbelib but a security-relevant divergence from proper decoding.
3. **`adaptive.rs:161`** — `tm` can go negative; multiplying magnitudes by a negative scale produces sign-inverted audio, not a panic but audibly wrong. Clamp.
4. **`decode.rs:657–659`** — Tone-decoder magic tables `T5/T6/T7` have no golden reference; a single-bit transcription error would silently mis-decode tone-frame indices.
5. **`tables.rs:50–53`** — `W0_TABLE` doc claims filler for indices 120..=123; table content contradicts.
6. **`dc_rmv.rs:30–31`** — Doc says function panics; it doesn't.
7. **`enhance_wave` vs `enhance_live` padding paths differ** for `samples.len() < 128`. Both zero-fill on OOB but not identically. Unlikely to bite (RELEASE_MIN gate) but not tested.

Decoder-panic-on-hostile-bits risk: **low**. The `.get()/unwrap_or` discipline is thorough throughout the decode path. The ECC parity-bit blind spot in item 2 is the only real correctness concern for the decoder on RF input; encoder debug-panic (item 1) is easily reachable but only from local misuse of `VuvDecisions`.
