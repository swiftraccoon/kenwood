# DSP + Audio Apps Review

Scope: `if-dsp/`, `thd75-listen/`, `sextant/`, `stargazer/`.

## `if-dsp`

- `if-dsp/src/spectrum.rs:120` — `write_psd` returns raw averaged squared magnitudes but the doc calls it a "one-sided PSD"; there is no window-power compensation and no factor-of-2 doubling for bins between DC and Nyquist. Absolute values will be wrong for anyone using them as PSD; it is only proportional to signal power. Cosmetic if used only for peak-picking, but the label is misleading.
- `if-dsp/src/channelizer.rs:114` — `AmDemod::new(filter_hz, BASEBAND_RATE, DECIM_TAPS)` gives AM only 63 mode-filter taps while SSB/CW get 255 (`MODE_TAPS`). Passband skirts on AM will be much softer than on SSB/CW; likely deliberate but nowhere documented, and the constant name is misleading (it is the decimator tap count reused as the mode LPF tap count).
- `if-dsp/src/fir.rs:100` and 130, 160 — `hist.rotate_right(1)` runs O(N) per sample on every push. For the SSB mode filter (255 complex taps × 12 kHz = ~3M cmul/s per channel) this is measurable CPU. Correct, just slow; a ring index would eliminate the memmove.
- `if-dsp/src/nco.rs:37` — single-shot phase wrap works only when `|step| ≤ π`, i.e. `|freq_hz| ≤ sample_rate/2`. All callers satisfy that today, but nothing enforces it, and a future high-frequency NCO would drift silently.
- `if-dsp/src/fir.rs:26,68` — `assert!(taps % 2 == 1)` and `assert!(hi_hz > lo_hz)`. Panic surfaces if a caller ever passes user-derived tap counts or a swapped band. Not currently reachable from user input.
- `if-dsp/src/agc.rs:29` — the AGC tracks instantaneous `|s|` rather than a smoothed envelope; on a low-frequency tone the gain modulates within a cycle, adding harmonic distortion at very low signal levels. Standard behavior for a peak-tracking AGC and hidden by AGC's own smoothing on speech, but worth knowing.

All FIR/decimator/interpolator/NCO math (windowed-sinc lowpass, complex-modulated bandpass, zero-stuffing interpolation with `factor` gain compensation, real-mix halving compensated by the 2× in `SsbDemod`) is otherwise correct and well-tested.

## `thd75-listen`

- `thd75-listen/src/main.rs:532` — `tracing_error` swallows every cpal `StreamError` silently ("just count on stderr-free best effort"). If the input device is unplugged the callback stops firing and there is no user-visible signal beyond `in_blocks` freezing (and the prompt does not periodically show that). Underrun accounting only fires while the output stream still runs.
- `thd75-listen/src/main.rs:326` — overrun counter is bumped when `tx_in.try_send` fails; the dropped chunk is lost silently past the once-per-session `underrun_notice`. Fine given the accessibility spec, but the pair (overrun / underrun) is only visible via `status`.
- `thd75-listen/src/main.rs:322` — mono fold takes the first channel only (`frame.first()`); if the radio's audio device ever exposes stereo, right-channel content is silently discarded. Documented as "Mono IF".

Pipeline math and pacing (48 k input, 4× decimate to 12 k, mode filter, 4× interpolate to 48 k, 20 dB/decade dBFS meter with 0.2/0.8 smoothing) is consistent with `if-dsp`.

## `sextant`

- `sextant/src/audio.rs:1416` — `resample_buffer` zero-pads the final chunk to `CHUNK=1024`, so playing or transmitting a WAV appends up to ~21 ms (1024/48 k) of silence at the end. Harmless for playback, but a transmitted file will end with padded silence encoded as AMBE.
- `sextant/src/audio.rs:1431` — `reader.samples::<i16>().filter_map(Result::ok)`: any WAV that is not 16-bit PCM produces zero samples and is played/transmitted as silence, with no error surfaced (the comment even admits this). Users get a mysterious "PTT works but nothing goes out" if they pick a 24-bit file.
- `sextant/src/audio.rs:1591` — `push_speaker` silently drops samples on ringbuf overflow (`return` inside the loop) with no counter. The audio worker cannot tell the operator when the speaker path is dropping frames.
- `sextant/src/audio.rs:1746`, `1768`, `1810` — output callbacks `try_pop().unwrap_or(0.0)` on empty ringbuf — silent underrun, no counter, unlike thd75-listen which at least keeps one.
- `sextant/src/audio.rs:1573` — `AudioIo::pop_mic` first checks `occupied_len() < n` then does `n` pops; if a pop returns `None` mid-loop it discards the partially-filled buffer. Cannot happen with the SPSC ring (producer only grows), but the failure branch would drop up to `n-1` mic samples if the ring ever went MPSC.
- `sextant/src/audio.rs:946` — the audio worker `std::thread::sleep(5 ms)` even when commands are queued; net worst-case dispatch latency is 5 ms which is fine, but a `recv_timeout(5ms)` would be strictly better.
- `sextant/src/audio.rs:1119` — RX i16→f32 uses `/32768.0` and the reverse uses `*32768.0` clamped to `-32767`. Asymmetric but standard (a +1.0 input maps to +32767, a -1.0 to -32767). The 20 comments describing these casts are credible.
- `sextant/src/audio.rs:70` — `RX_PRIME_FRAMES_ENHANCED = 1` is compensated for the live enhancer's ~3.2-frame release floor; the reasoning is documented and matches the router tests.
- `sextant/src/audio.rs:189` — `apply_fade` uses `PI * i / RAMP_LEN` (half-cycle raised cosine) — a proper Hann-shaped fade-in / fade-out. Correct.
- `sextant/src/session.rs:588` — `classify_gap` (also at line 319) computes `(seq + SUPERFRAME_LEN - expected) % SUPERFRAME_LEN`. `expected_seq` is stored `(seq+1) % 21`, so 20→0 is treated as InOrder and small backtracks (`gap >= 18`) become `Late`. Correct.
- `sextant/src/session.rs:837` — TX seq wraps `mod 21`, avoiding the 0x40 EOT bit trap the doc mentions. Fine.
- `sextant/src/app.rs:288, 388, 492` — GUI drain functions use `try_recv`; no channel-blocking on the UI thread. Runtime spawns for HTTP fetches (`hosts::fetch_directory`, `fetch_auth_directory`) go through `runtime.spawn`, not `block_on`. Good hygiene.
- `sextant/src/settings.rs:275`, `hosts.rs:269`, `heard.rs:189` — synchronous small-file writes from `App::handle_status` (i.e. from the egui update loop). Fine for a few KB but they are on the render path.
- No `unwrap()`, `todo!()`, `unimplemented!()`, `panic!()`, or `TODO/FIXME/HACK` anywhere in `sextant/src` — very clean.

Sextant/session's slow-data GPS assembler (`session.rs:412`) treats `seq==0` as a superframe resync (discards the half-block) and requires two halves of a `0x3X` block before parsing DPRS. Matches the sending side.

## `stargazer`

- `stargazer/src/writer.rs:280-286` — the output directory is built from `rec.origin.reflector` and `rec.origin.module.as_char()`. `module` is a validated `Module` (single letter). `reflector` however is a `String` field on `StreamOrigin`, produced by `session.rs` from the target config (trusted) and by `dvrec.rs:344` from the dvrec importer (untrusted-shape file name — but constrained by `harvest::split_target` to `<SYSTEM>-<UPPERCASE-LETTER>` before it becomes a `system: String`). If someone ever hand-crafts a config with `reflector = "../evil"`, that lands directly in the recordings path. Sanitizing the reflector name the way `sanitize_callsign` does for the callsign field would close it.
- `stargazer/src/harvest.rs:918` — `fetch` calls `resp.bytes().await` with no size cap. A hostile or broken server can force unbounded memory allocation on the harvester (which by policy runs unattended). A ~5 MB cap on `.dvrec` bodies would match the file class.
- `stargazer/src/harvest.rs:240-247` — `is_safe_basename` (the recent fix): validates the *decoded* href as a single `Component::Normal`, and also forbids `\`. Correct. Combined with `write_atomic(&published_dir.join(&item.file_name), …)` at 883 this is safe against `%2Fetc%2Fpasswd`, `..%2F..`, and Windows-flavored `..\..\`.
- `stargazer/src/capture.rs:32` — `seq_gap` is now the single source of truth; `dvrec.rs:202`, `features.rs:124`, `audio.rs:38` all call it. Out-of-alphabet bytes (0x40 EOT flag riding the seq) return zero — the intended fix. Both tests exercise this. No divergence remains.
- `stargazer/src/control.rs:396-412` — accept-loop counts transient errors; `MAX_ACCEPT_FAILURES=8` with a 50 ms sleep between them means ~400 ms of sustained error before shutdown. Matches the "control plane must not take the data plane down" fix.
- `stargazer/src/harvest.rs:66-100` — `robots_disallows` implements a minimal robots.txt group parser; correctly resets the group on a fresh `User-agent` block. A `Disallow:` with an empty value is treated as "no restriction" (`!value.is_empty()`), which matches the spec.
- `stargazer/src/audio.rs:26` — `decode_stream` builds a fresh `AmbeDecoder` per stream (correct; per the module doc, adaptive smoothing must not bleed).
- `stargazer/src/writer.rs:334-349` — `sanitize_callsign` collapses non-`[A-Z0-9]` to `-` and maps all-dash to `UNKNOWN`. Safe.
- `stargazer/src/survey.rs:105-119` — HTML tag stripper is a hand-roll that does not understand quoted attributes containing `>`. Fine for the DPLUSMON table but brittle against arbitrary HTML — not user-facing, so acceptable.
- `stargazer/src/wav.rs:16` — WAV writer uses `saturating_mul`/`saturating_add` for size fields; safe against overflow, doc admits values are unreachable.

No `unwrap`, `todo!`, `unimplemented!`, `panic!`, or `TODO/FIXME/HACK` in the entire `stargazer/src`, `sextant/src`, or `if-dsp/src` trees.

## `#[expect]`/`#[allow]` audit

All 20 attribute uses in `sextant/src/audio.rs` are `cast_precision_loss` / `cast_possible_truncation` on numeric conversions where the source range is bounded (channel counts 1–8, i16 samples clamped, ramp indices <80, `f32` gains ≤1.0). Reasons are concrete and check out.

All 24 in `if-dsp/src` are similarly narrow: `usize`→`f64/f32` conversions of tap indices and sample lengths that fit exactly (well under 2²³), and `f64` sinusoidal values narrowing to `f32`. Also credible.

The one `too_many_lines` in `sextant/src/session.rs:668` is a `tokio::select!` main loop — the reason ("splitting would obscure the select! structure") is legitimate.

## Docs/comments

- `if-dsp/src/spectrum.rs:114` — "averaged one-sided PSD" is misleading (see above).
- `if-dsp/src/channelizer.rs:74-76` — comment reads "Taps for the mode passband (at the baseband rate)" for `MODE_TAPS = 255`, but `AmDemod` is built with `DECIM_TAPS = 63` at 114. Either use `MODE_TAPS` for AM too or explain the exception near the constants.
- `sextant/src/audio.rs:1424-1426` — "16-bit PCM only — samples that don't decode as `i16` are dropped (a non-16-bit file therefore plays as silence rather than an error)" — accurately documents the silent-failure behavior. Consider surfacing an error to the GUI instead.

## Dead code / half-wired features

- `stargazer` exposes voice-capture components (`audio.rs::decode_stream`, `wav.rs`, `writer.rs` WAV path) but the recorder actually calls them via `writer.write()` when `write_wav = true`. Not dead; the "unwired voice-capture components" in the task brief appear to be wired.
- No pretending-to-work features found (no stubbed handlers, no orphan enums, no `warn`-swallowed unimplemented branches).

Overall assessment: the DSP math in `if-dsp` is correct; the `sextant` router/audio worker has thorough tests for its non-obvious invariants (crossfades, holdback, priming compensation); `stargazer`'s recent path-traversal / seq-gap / accept-loop fixes are solid and shared correctly across the four seq-gap sites. The two most substantive gaps left are the untracked speaker underruns in `sextant/src/audio.rs`, the silent-failure playback of non-i16 WAVs, and the unbounded response-body read in `stargazer/src/harvest.rs:918` running unattended by cron.
