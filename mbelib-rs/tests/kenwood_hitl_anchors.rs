// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Hardware-in-the-loop verification: every test in this file parses
// real AMBE byte streams captured from a TH-D75 in D-STAR TX mode and
// asserts that the captured data matches the structural and bit-level
// findings codified in `src/encode/kenwood/anchors.rs`. The captures
// in tests/fixtures/thd75/ are lawful artifacts of running
// `AMBE_CAPTURE=<path>` on `thd75-repl` against an owned radio per
// DMCA §1201(f).

//! Hardware-in-the-loop bit-exact verification of the AMBE encoder
//! against radio-captured anchors.
//!
//! These tests exist on two tiers:
//!
//! 1. **Capture-integrity tests** (always run with this feature):
//!    parse the fixture .ambe files, verify each is a multiple of 9
//!    bytes (no header), verify the steady-state lock at the four
//!    pitch anchors holds at the documented quality, verify the
//!    volatile/stable bit partition.
//!
//! 2. **Encoder-vs-anchor tests** (`#[ignore]`'d until the encoder is
//!    Kenwood-perfect): synthesize a sinusoidal PCM stream at the
//!    anchor frequency, run it through the Rust encoder, mask out
//!    protocol-overhead bits, assert byte-for-byte equality with the
//!    captured anchor.
//!
//! Run all (including ignored) with:
//! ```text
//! cargo test -p mbelib-rs --features kenwood-tables -- --include-ignored
//! ```

#![cfg(feature = "kenwood-tables")]
#![expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "Tone-synthesis and lock-quality math throughout this file casts sample \
              indices, frame counts, and clamped f32 samples between numeric types; \
              bounds are guaranteed by the synthesis parameters (amplitudes <= i16::MAX, \
              counts <= a few hundred frames)."
)]

// Dev-dependencies pulled in by sibling tests. Acknowledge them here
// so `unused_crate_dependencies` stays silent for this compilation
// unit.
use proptest as _;
use realfft as _;
use wide as _;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use mbelib_rs::kenwood::anchors::{
    FRAME_LEN, PITCH_ANCHORS, PitchAnchor, STABLE_BIT_MASK, VOLATILE_BIT_MASK, anchor_for,
    hamming_distance, mask_stable_bits,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ─── fixture loading ────────────────────────────────────────────────

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("thd75")
}

fn load_capture(name: &str) -> Result<Vec<u8>, String> {
    let path = fixtures_dir().join(name);
    fs::read(&path).map_err(|err| format!("failed to read fixture {}: {err}", path.display()))
}

/// Parse a capture file into 9-byte frames. Captures are raw AMBE byte
/// streams with no header — frames begin at byte 0. Returns `None` if
/// the file is empty (happens with very-short keying — e.g. `capture_6`
/// which the encoder never flushed).
fn parse_frames(raw: &[u8]) -> Option<Vec<[u8; FRAME_LEN]>> {
    if raw.is_empty() {
        return None;
    }
    Some(
        raw.chunks_exact(FRAME_LEN)
            .filter_map(|chunk| chunk.try_into().ok())
            .collect(),
    )
}

/// Apply [`STABLE_BIT_MASK`] to a frame.
fn mask(frame: [u8; FRAME_LEN]) -> [u8; FRAME_LEN] {
    mask_stable_bits(frame)
}

/// Count occurrences of each masked frame in the second half of the
/// stream (skipping the encoder's transient).
fn dominant_in_second_half(frames: &[[u8; FRAME_LEN]]) -> Option<([u8; FRAME_LEN], usize, usize)> {
    let half = frames.len() / 2;
    let second = frames.split_at(half).1;
    let mut counts: HashMap<[u8; FRAME_LEN], usize> = HashMap::new();
    for &frame in second {
        *counts.entry(mask(frame)).or_insert(0) += 1;
    }
    let (dom_frame, dom_count) = counts.into_iter().max_by_key(|&(_, n)| n)?;
    Some((dom_frame, dom_count, second.len()))
}

// ─── capture-integrity tests (always enabled with feature) ──────────

#[test]
fn captures_have_no_header_and_divide_by_nine() -> TestResult {
    // Each capture is a raw AMBE byte stream — no header, frames
    // begin at byte 0. File sizes divide evenly by 9 (one frame).
    let names = [
        "capture_0_voice.ambe",
        "capture_1_just_keyed.ambe",
        "capture_2_440Hz_tone.ambe",
        "capture_3_320Hz_tone.ambe",
        "capture_4_210Hz_tone.ambe",
        "capture_5_100Hz_tone.ambe",
        "capture_7_550Hz_tone.ambe",
        "capture_8_660Hz_tone.ambe",
        "capture_9_mic_covered.ambe",
        "capture_10_mic_covered.ambe",
    ];
    for name in names {
        let raw = load_capture(name)?;
        assert_eq!(
            raw.len() % FRAME_LEN,
            0,
            "{name}: size {} is not a multiple of {FRAME_LEN}",
            raw.len()
        );
    }
    Ok(())
}

#[test]
fn empty_capture_for_too_short_keying() -> TestResult {
    // capture_6 was an intentional very-brief key-up; the encoder
    // didn't get to flush any frames. Documents the firmware behaviour.
    let raw = load_capture("capture_6_shorter_keying.ambe")?;
    assert_eq!(
        raw.len(),
        0,
        "capture_6 expected to be empty (TX too short to flush encoder)"
    );
    Ok(())
}

#[test]
fn frame_stride_is_nine_bytes() -> TestResult {
    // The 440 Hz capture is the strongest demonstration: 107/196
    // frames are byte-identical when sliced at stride 9.
    let frames =
        parse_frames(&load_capture("capture_2_440Hz_tone.ambe")?).ok_or("440 Hz capture parses")?;
    let mut counts: HashMap<[u8; FRAME_LEN], usize> = HashMap::new();
    for &frame in &frames {
        *counts.entry(frame).or_insert(0) += 1;
    }
    let (_, dom_count) = counts
        .iter()
        .max_by_key(|&(_, &n)| n)
        .ok_or("non-empty capture")?;
    assert!(
        *dom_count >= 100,
        "expected >=100 byte-identical frames at stride 9, got {dom_count}"
    );
    Ok(())
}

#[test]
fn pitch_anchors_match_capture_steady_state() -> TestResult {
    // For each anchor, the corresponding capture's 2nd-half-dominant
    // masked frame must match the codified anchor.frame, at no less
    // than the documented lock_quality.
    let anchor_files: &[(f32, &str)] = &[
        (210.0, "capture_4_210Hz_tone.ambe"),
        (440.0, "capture_2_440Hz_tone.ambe"),
        (550.0, "capture_7_550Hz_tone.ambe"),
        (660.0, "capture_8_660Hz_tone.ambe"),
    ];
    for &(freq, file) in anchor_files {
        let frames = parse_frames(&load_capture(file)?).ok_or("capture parses")?;
        let (dom, count, total) = dominant_in_second_half(&frames).ok_or("non-empty 2nd half")?;
        let measured_quality = count as f32 / total as f32;
        let anchor = anchor_for(freq).ok_or("anchor exists")?;
        assert_eq!(
            dom, anchor.frame,
            "{freq} Hz: 2nd-half dominant masked frame {dom:02x?} != codified anchor {:02x?}",
            anchor.frame
        );
        assert!(
            measured_quality >= anchor.lock_quality - 0.01,
            "{freq} Hz: measured lock quality {measured_quality:.3} below codified {:.3}",
            anchor.lock_quality
        );
    }
    Ok(())
}

#[test]
fn unsupported_pitches_do_not_lock() -> TestResult {
    // 100 Hz and 320 Hz captures exist but didn't lock — phone-speaker
    // limitations, not codec behaviour. Document this explicitly so
    // future sessions don't try to derive anchors from these files.
    for file in ["capture_5_100Hz_tone.ambe", "capture_3_320Hz_tone.ambe"] {
        let frames = parse_frames(&load_capture(file)?).ok_or("capture parses")?;
        let (_, count, total) = dominant_in_second_half(&frames).ok_or("non-empty 2nd half")?;
        let quality = count as f32 / total as f32;
        assert!(
            quality < 0.5,
            "{file}: unexpected lock at quality {quality:.2} — anchor may be derivable"
        );
    }
    Ok(())
}

#[test]
fn mic_covered_captures_show_no_silence_anchor() -> TestResult {
    // Both mic-covered captures still produce 100% unique frames in
    // the 2nd half. Documents that this radio has no quiescent silence
    // pattern — mic noise always drives V/UV decisions.
    for file in ["capture_9_mic_covered.ambe", "capture_10_mic_covered.ambe"] {
        let frames = parse_frames(&load_capture(file)?).ok_or("capture parses")?;
        let (_, count, _total) = dominant_in_second_half(&frames).ok_or("non-empty 2nd half")?;
        assert!(
            count <= 2,
            "{file}: mic-covered capture showed unexpected repetition (count={count})"
        );
    }
    Ok(())
}

#[test]
fn volatile_bits_are_actually_volatile_in_440hz_capture() -> TestResult {
    // For each bit position in VOLATILE_BIT_MASK, at least 5% of 440
    // Hz frames must show that bit differing from the dominant — that's
    // why we classified it as volatile. Catches mask drift.
    let frames =
        parse_frames(&load_capture("capture_2_440Hz_tone.ambe")?).ok_or("440 Hz capture parses")?;
    // Mid-stream reference frame, deep in steady state.
    let ref_frame = *frames.get(60).ok_or("capture has at least 61 frames")?;
    let n = frames.len();
    for (byte_idx, (&volatile, &ref_byte)) in
        VOLATILE_BIT_MASK.iter().zip(ref_frame.iter()).enumerate()
    {
        for bit in 0_u8..8 {
            if volatile & (1 << bit) == 0 {
                continue;
            }
            let mask_bit = 1 << bit;
            let differ = frames
                .iter()
                .filter(|f| {
                    f.get(byte_idx)
                        .is_some_and(|&b| b & mask_bit != ref_byte & mask_bit)
                })
                .count();
            let pct = 100 * differ / n;
            assert!(
                pct >= 5,
                "byte {byte_idx} bit {bit}: only {pct}% volatile, mask says it should be"
            );
        }
    }
    Ok(())
}

#[test]
fn stable_bits_are_actually_stable_in_440hz_capture() -> TestResult {
    // For each bit in STABLE_BIT_MASK, < 5% of 440 Hz frames may differ
    // from the dominant masked frame. Confirms our partition is correct.
    let frames =
        parse_frames(&load_capture("capture_2_440Hz_tone.ambe")?).ok_or("440 Hz capture parses")?;
    let dom = anchor_for(440.0).ok_or("440 Hz anchor")?.frame;
    let n = frames.len();
    for (byte_idx, (&stable, &dom_byte)) in STABLE_BIT_MASK.iter().zip(dom.iter()).enumerate() {
        for bit in 0_u8..8 {
            if stable & (1 << bit) == 0 {
                continue;
            }
            let mask_bit = 1 << bit;
            let differ = frames
                .iter()
                .filter(|f| {
                    f.get(byte_idx)
                        .is_some_and(|&b| b & mask_bit != dom_byte & mask_bit)
                })
                .count();
            let pct = 100 * differ / n;
            assert!(
                pct < 5,
                "byte {byte_idx} bit {bit}: {pct}% volatile despite STABLE_BIT_MASK"
            );
        }
    }
    Ok(())
}

// ─── encoder-vs-anchor tests (ignored until encoder is Kenwood-perfect) ─

/// Synthesize a steady sinusoidal PCM stream at the given frequency.
///
/// 8 kHz sample rate, 16-bit signed. Length is `n_frames * 160`
/// because AMBE/D-STAR uses 20 ms (160-sample) frames at 8 kHz.
/// No additive noise — with `D_STAR_GAIN_ADJUST = 0.0`, pure sine
/// produces `b2 = 49` for 210 Hz, within 1 step of Kenwood's `b2 = 48`.
/// Adding a noise floor pushes b2 higher and away from the anchor.
fn synthesize_tone_pcm(frequency_hz: f32, n_frames: usize, amplitude: i16) -> Vec<i16> {
    synthesize_tone_with_noise(frequency_hz, n_frames, amplitude, 0)
}

/// Synthesize a tone with a configurable additive noise floor.
fn synthesize_tone_with_noise(
    frequency_hz: f32,
    n_frames: usize,
    amplitude: i16,
    noise_amplitude: i16,
) -> Vec<i16> {
    const SAMPLE_RATE: f32 = 8000.0;
    const FRAME_SAMPLES: usize = 160;
    let n_samples = n_frames * FRAME_SAMPLES;
    let omega = 2.0 * core::f32::consts::PI * frequency_hz / SAMPLE_RATE;
    let amp = f32::from(amplitude);
    let noise_amp = f32::from(noise_amplitude);
    let mut rng_state: u32 = 0xDEAD_BEEF;
    (0..n_samples)
        .map(|i| {
            // Xorshift32 PRNG → noise sample in [-1, 1]
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 17;
            rng_state ^= rng_state << 5;
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_wrap,
                reason = "u32-to-f32 for normalising xorshift output to ±1; precision loss is \
                          irrelevant for noise generation."
            )]
            let noise_unit = (rng_state as i32 as f32) / f32::from(i16::MAX) / 65536.0;
            let s = (i as f32 * omega)
                .sin()
                .mul_add(amp, noise_unit * noise_amp);
            // Clamp to i16 range. Soft sin() bounded by amp ≤ i16::MAX
            // means saturation is theoretical but cheap to be safe.
            s.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
        })
        .collect()
}

/// Run the Rust encoder on PCM and return the emitted 9-byte AMBE
/// frames. Slices the input into 160-sample (20 ms) frames matching the
/// AMBE frame rate at 8 kHz. Trailing samples (less than one full
/// frame) are dropped — synthesized inputs in this test are sized to
/// be exact multiples of 160.
fn encode_to_ambe_frames(pcm: &[i16]) -> Vec<[u8; FRAME_LEN]> {
    use mbelib_rs::AmbeEncoder;

    const SAMPLES_PER_FRAME: usize = 160;
    let mut encoder = AmbeEncoder::new_with_lookahead();
    pcm.chunks_exact(SAMPLES_PER_FRAME)
        .map(|chunk| encoder.encode_frame_i16(chunk))
        .collect()
}

/// The "Kenwood-exact" milestone — V path: synthesized 210 Hz tone
/// must produce a bit-exact-after-mask AMBE frame matching the
/// 210 Hz anchor. Only 210 Hz is currently a reachable target — the
/// 440/550/660 Hz inputs have pitch periods below the OP25 pitch
/// tracker's 21-sample minimum (`PitchAnchor::in_op25_pitch_range`
/// flags this); the encoder octave-folds those inputs, producing a
/// different b0 than Kenwood. Widening `PITCH_CANDIDATES` to cover
/// the full IMBE-spec range is a separate larger refactor.
///
/// Currently `#[ignore]`'d — the encoder pipeline is at 27 wrong bits
/// out of 58 stable for 210 Hz (47% wrong). This test gates the
/// V-path milestone.
#[test]
#[ignore = "encoder not yet Kenwood-exact; gates the V-path milestone (210 Hz only)"]
fn rust_encoder_matches_pitch_anchors() -> TestResult {
    for anchor in PITCH_ANCHORS.iter().filter(|a| a.in_op25_pitch_range) {
        rust_encoder_matches_one_anchor(anchor)?;
    }
    Ok(())
}

/// Aspirational test for full IMBE pitch range support. Currently
/// stays `#[ignore]`'d well past the V-path milestone — gates the
/// full-range encoder rewrite.
#[test]
#[ignore = "blocked on widening PITCH_CANDIDATES to full IMBE range (~50-625 Hz)"]
fn rust_encoder_matches_high_pitch_anchors() -> TestResult {
    for anchor in PITCH_ANCHORS.iter().filter(|a| !a.in_op25_pitch_range) {
        rust_encoder_matches_one_anchor(anchor)?;
    }
    Ok(())
}

/// Distinguish "ECC fallback" from "real encoding" by decoding MANY
/// distinct frames from a single capture and seeing if b values vary.
/// If all frames in a single 210 Hz capture decode to identical
/// b0/b1/L after dewhitening, our dewhitening produces nonsense and
/// ECC is fabricating a fallback.
///
/// Run with `cargo test ... -- --ignored --nocapture
/// dewhiten_variance_within_one_capture`.
#[test]
#[ignore = "diagnostic; tests if dewhitening is real or ECC fallback"]
fn dewhiten_variance_within_one_capture() -> TestResult {
    use mbelib_rs::decode_trace;
    let whiten: [u8; 4] = [0x70, 0x4F, 0x93, 0x40];

    let raw = load_capture("capture_4_210Hz_tone.ambe")?;
    let frames = parse_frames(&raw).ok_or("210 Hz capture parses")?;

    println!();
    println!("Decoding 20 distinct dewhitened frames from 210 Hz capture:");
    let mut seen = HashMap::<(usize, usize, usize), usize>::new();
    for (idx, &raw_frame) in frames.iter().enumerate().step_by(5).take(40) {
        let mut dewhitened = raw_frame;
        for (byte, &w) in dewhitened.iter_mut().zip(whiten.iter().cycle()) {
            *byte ^= w;
        }
        let (b, _, l, _) = decode_trace(&dewhitened);
        let key = (b[0], b[1], l);
        *seen.entry(key).or_insert(0) += 1;
        if idx < 30 {
            println!(
                "  frame[{:>3}] raw={} → dewhit={} → b0={:>3} b1={:>2} L={}",
                idx,
                hex9(&raw_frame),
                hex9(&dewhitened),
                b[0],
                b[1],
                l
            );
        }
    }
    println!();
    println!("Distinct (b0, b1, L) tuples seen: {}", seen.len());
    for ((b0, b1, l), n) in &seen {
        println!("  ({b0:>3}, {b1:>2}, {l:>2}) × {n}");
    }
    Ok(())
}

/// Verify the firmware whitening hypothesis on ALL 4 anchors. If
/// XOR with cycled 0x704F9340 produces sensible b0/L for every anchor
/// AND closely matches our self-encoded values, the whitener IS the
/// wire-format adapter.
///
/// Run with `cargo test ... -- --ignored --nocapture
/// confirm_whitener_on_all_anchors`.
#[test]
#[ignore = "diagnostic; verifies whitener hypothesis across all 4 anchors"]
fn confirm_whitener_on_all_anchors() {
    use mbelib_rs::decode_trace;
    let whiten: [u8; 4] = [0x70, 0x4F, 0x93, 0x40];

    println!();
    println!("Whitener hypothesis: XOR Kenwood raw with 0x704F9340 cycled, then decode.");
    println!(
        "  freq | kenwood raw            | dewhitened             | b0  L  | rust self-encode  rust b0/L"
    );
    for anchor in PITCH_ANCHORS {
        let mut dewhitened = anchor.raw_dominant_frame;
        for (byte, &w) in dewhitened.iter_mut().zip(whiten.iter().cycle()) {
            *byte ^= w;
        }
        let (b, _, l, _) = decode_trace(&dewhitened);

        println!(
            "  {:>4} | {} | {} | b0={:>3} b1={:>2} b2={:>2} b3={:>3} b4={:>3} L={}",
            anchor.frequency_hz,
            hex9(&anchor.raw_dominant_frame),
            hex9(&dewhitened),
            b[0],
            b[1],
            b[2],
            b[3],
            b[4],
            l,
        );
    }
    println!();
    println!("Self-encoded outputs for comparison:");
    for anchor in PITCH_ANCHORS {
        let pcm = synthesize_tone_pcm(anchor.frequency_hz, 150, 16384);
        let frames = encode_to_ambe_frames(&pcm);
        let mid = frames.get(100).copied().unwrap_or([0; 9]);
        let (b, _, l, _) = decode_trace(&mid);
        println!(
            "  {:>4} |                         {} | b0={:>3} b1={:>2} b2={:>2} b3={:>3} b4={:>3} L={}",
            anchor.frequency_hz,
            hex9(&mid),
            b[0],
            b[1],
            b[2],
            b[3],
            b[4],
            l,
        );
    }
}

/// Test simple wire-format permutations on the Kenwood 210 Hz capture
/// to see if any produce a sensible b0 (~86 like our self-roundtrip).
/// If a transformation yields b0 in the voice range with reasonable L,
/// that's a candidate Kenwood↔DSD wire-format adapter.
///
/// Run with `cargo test ... -- --ignored --nocapture probe_kenwood_wire_format`.
#[test]
#[ignore = "diagnostic; brute-search simple wire-format permutations"]
fn probe_kenwood_wire_format() -> TestResult {
    use mbelib_rs::decode_trace;
    let kenwood_raw = anchor_for(210.0).ok_or("anchor")?.raw_dominant_frame;
    println!();
    println!("Probing simple wire-format permutations of Kenwood 210 Hz capture:");
    println!("  raw bytes: {}", hex9(&kenwood_raw));

    // Helper: try a transformation, decode, print result
    let try_one = |label: &str, transformed: [u8; 9]| {
        let (b, _, l, _) = decode_trace(&transformed);
        let b0_voice = b[0] < 120;
        let l_plausible = l > 0 && l <= 56;
        let marker = if b0_voice && l_plausible {
            " ← PLAUSIBLE"
        } else {
            ""
        };
        println!(
            "  {label:<22} → {} → b0={:>3} L={:>3}{marker}",
            hex9(&transformed),
            b[0],
            l
        );
    };

    // 1. Identity (baseline)
    try_one("identity", kenwood_raw);

    // 2. Reverse byte order
    let mut rev_bytes = kenwood_raw;
    rev_bytes.reverse();
    try_one("byte-reversed", rev_bytes);

    // 3. Bit-reverse each byte (MSB↔LSB within byte)
    let mut bit_rev = [0_u8; 9];
    for (dst, &b) in bit_rev.iter_mut().zip(kenwood_raw.iter()) {
        *dst = b.reverse_bits();
    }
    try_one("bit-reversed-per-byte", bit_rev);

    // 4. Bit-reverse all 72 bits as one stream
    let mut all_bits_rev = [0_u8; 9];
    for (dst, &b) in all_bits_rev.iter_mut().rev().zip(kenwood_raw.iter()) {
        *dst = b.reverse_bits();
    }
    try_one("bits-reversed-all", all_bits_rev);

    // 5. XOR with 0x704F9340 cycled (firmware whitening pattern)
    let whiten: [u8; 4] = [0x70, 0x4F, 0x93, 0x40];
    let mut whitened = [0_u8; 9];
    for (dst, (&b, &w)) in whitened
        .iter_mut()
        .zip(kenwood_raw.iter().zip(whiten.iter().cycle()))
    {
        *dst = b ^ w;
    }
    try_one("XOR 0x704F9340 cycled", whitened);

    // 6. Swap nibbles per byte
    let mut nib_swap = [0_u8; 9];
    for (dst, &b) in nib_swap.iter_mut().zip(kenwood_raw.iter()) {
        *dst = b.rotate_left(4);
    }
    try_one("nibble-swapped-per-byte", nib_swap);
    Ok(())
}

/// Sanity check: decode our OWN encoder output and confirm it round-trips
/// to sensible b0..b8 values. If this works but the Kenwood capture
/// decode produces b0=126 (erasure), Kenwood uses a different wire
/// format than DSD/mbelib.
///
/// Run with `cargo test ... -- --ignored --nocapture self_round_trip_b_fields`.
#[test]
#[ignore = "diagnostic; verifies our encoder→decoder round-trip"]
fn self_round_trip_b_fields() -> TestResult {
    use mbelib_rs::decode_trace;

    println!();
    println!("Self round-trip (synthesize 210 Hz → encode → decode_trace):");
    let pcm = synthesize_tone_pcm(210.0, 150, 16384);
    let frames = encode_to_ambe_frames(&pcm);
    if frames.is_empty() {
        return Ok(());
    }
    // Mid-stream frame
    let mid = frames
        .get(100)
        .ok_or("encoder emitted at least 101 frames")?;
    let (b, w0, l, _) = decode_trace(mid);
    println!("  raw frame: {}", hex9(mid));
    println!(
        "  b0={} b1={} b2={} b3={} b4={} b5={} b6={} b7={} b8={}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]
    );
    println!("  w0={w0:.4} L={l}");
    println!();
    println!("Same operation on Kenwood 210 Hz capture's dominant frame:");
    let kenwood = anchor_for(210.0).ok_or("anchor")?.raw_dominant_frame;
    let (b, w0, l, _) = decode_trace(&kenwood);
    println!("  raw frame: {}", hex9(&kenwood));
    println!(
        "  b0={} b1={} b2={} b3={} b4={} b5={} b6={} b7={} b8={}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]
    );
    println!("  w0={w0:.4} L={l}");
    Ok(())
}

/// Confirm whether b2=0 is universal (encoder bug) or specific to pure
/// sines (degenerate input). Tries several non-sinusoidal inputs.
///
/// Run with `cargo test ... -- --ignored --nocapture probe_b2_with_varied_inputs`.
#[test]
#[ignore = "diagnostic; checks if b2=0 is a pure-sine artifact"]
fn probe_b2_with_varied_inputs() {
    use mbelib_rs::decode_trace;

    println!();
    println!("Probing b2 (gain) across different input types:");
    println!("  input                     |  rust b0 b1 b2 b3 L");

    let dump = |label: &str, pcm: &[i16]| {
        let frames = encode_to_ambe_frames(pcm);
        if frames.is_empty() {
            println!("  {label:<26} | no frames");
            return;
        }
        let half = frames.len() / 2;
        let mut counts: HashMap<[u8; FRAME_LEN], usize> = HashMap::new();
        for &f in frames.split_at(half).1 {
            *counts.entry(f).or_insert(0) += 1;
        }
        let Some((dom, _)) = counts.iter().max_by_key(|&(_, &n)| n) else {
            println!("  {label:<26} | empty second half");
            return;
        };
        let (b, _, l, _) = decode_trace(dom);
        println!(
            "  {label:<26} |   {:>3} {:>2} {:>2} {:>3} {:>2}",
            b[0], b[1], b[2], b[3], l
        );
    };

    // 1. Pure sine 210 Hz
    dump("210 Hz sine 16384", &synthesize_tone_pcm(210.0, 150, 16384));

    // 2. Sum of two sines 210 + 420 Hz, each at 12000 amplitude
    let two_tone: Vec<i16> = (0..150 * 160)
        .map(|i| {
            let t = (i as f32) / 8000.0;
            let s1 = (210.0 * std::f32::consts::TAU * t).sin();
            let s2 = (420.0 * std::f32::consts::TAU * t).sin();
            let combined = (s1 + s2) * 12000.0;
            combined.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
        })
        .collect();
    dump("210+420 Hz two-tone", &two_tone);

    // 3. White noise full scale via xorshift
    let mut rng_state: u32 = 0xDEAD_BEEF;
    let noise: Vec<i16> = (0..150 * 160)
        .map(|_| {
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 17;
            rng_state ^= rng_state << 5;
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_possible_wrap,
                reason = "PRNG output to i16 PCM. The truncation is intentional — the \
                          xorshift32 produces a full 32-bit value and we want the low 16 \
                          as the PCM sample. Wrap is safe; sign loss is the desired noise \
                          characteristic."
            )]
            let v = (rng_state as i32 % i32::from(i16::MAX)) as i16;
            v
        })
        .collect();
    dump("white noise (xorshift)", &noise);

    // 4. AM-modulated 210 Hz at 30 Hz mod
    let am: Vec<i16> = (0..150 * 160)
        .map(|i| {
            let t = (i as f32) / 8000.0;
            let env = 0.5_f32.mul_add((30.0 * std::f32::consts::TAU * t).sin(), 0.5);
            let s = env * (210.0 * std::f32::consts::TAU * t).sin() * 24000.0;
            s.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
        })
        .collect();
    dump("210 Hz × 30 Hz AM", &am);
}

/// Sweep noise floor levels for 210 Hz tone to find b2-match.
///
/// Run with `cargo test ... -- --ignored --nocapture sweep_noise_floor`.
#[test]
#[ignore = "diagnostic; finds noise floor that matches Kenwood gain"]
fn sweep_noise_floor() {
    use mbelib_rs::decode_trace;

    println!();
    println!("Sweeping noise floor for 210 Hz tone (signal amp 16384):");
    println!("  noise | SNR(dB) | b0 b1 b2 b3 b4 L");
    let signal_amp: i16 = 16384;
    let noise_levels: [i16; 12] = [
        0, 32, 128, 512, 1024, 2048, 4096, 6144, 8192, 12288, 16384, 24576,
    ];
    for noise in noise_levels {
        let snr_db = if noise == 0 {
            f32::INFINITY
        } else {
            20.0 * (f32::from(signal_amp) / f32::from(noise)).log10()
        };
        let pcm = synthesize_tone_with_noise(210.0, 150, signal_amp, noise);
        let frames = encode_to_ambe_frames(&pcm);
        if frames.is_empty() {
            continue;
        }
        let half = frames.len() / 2;
        let mut counts: HashMap<[u8; FRAME_LEN], usize> = HashMap::new();
        for &f in frames.split_at(half).1 {
            *counts.entry(f).or_insert(0) += 1;
        }
        let Some((dom, _)) = counts.iter().max_by_key(|&(_, &n)| n) else {
            continue;
        };
        let (b, _, l, _) = decode_trace(dom);
        println!(
            "  {:>5} | {:>6.1}  | {:>3} {:>2} {:>2} {:>3} {:>3} {:>2}",
            noise, snr_db, b[0], b[1], b[2], b[3], b[4], l
        );
    }
}

/// Sweep input amplitudes for 210 Hz tone, decode the Rust encoder's
/// dominant output through `decode_trace`, and find the amplitude that
/// gets b2 (gain) closest to Kenwood's b2=48 reference.
///
/// Run with `cargo test ... -- --ignored --nocapture
/// sweep_amplitude_for_b2_match`.
#[test]
#[ignore = "diagnostic; finds input amplitude that matches Kenwood gain"]
fn sweep_amplitude_for_b2_match() {
    use mbelib_rs::decode_trace;

    // Kenwood's b2 for 210 Hz is 48 per dewhitened decode.
    const KENWOOD_B2: usize = 48;

    println!();
    println!("Sweeping input amplitudes to find Kenwood gain match:");
    println!("  amp   |  rust b0 b1 b2 b3 L  | b2 vs Kenwood (48)");
    let amplitudes: [i16; 9] = [1024, 4096, 8192, 12288, 16384, 20480, 24576, 28672, 32767];
    for amp in amplitudes {
        let pcm = synthesize_tone_pcm(210.0, 150, amp);
        let frames = encode_to_ambe_frames(&pcm);
        if frames.is_empty() {
            continue;
        }
        let half = frames.len() / 2;
        let mut counts: HashMap<[u8; FRAME_LEN], usize> = HashMap::new();
        for &f in frames.split_at(half).1 {
            *counts.entry(f).or_insert(0) += 1;
        }
        let Some((dom, _)) = counts.iter().max_by_key(|&(_, &n)| n) else {
            continue;
        };
        let (b, _, l, _) = decode_trace(dom);
        let dist = i32::try_from(b[2]).unwrap_or(0) - i32::try_from(KENWOOD_B2).unwrap_or(0);
        let marker = if b[2] == KENWOOD_B2 { " ← MATCH" } else { "" };
        println!(
            "  {:>5} |   {:>3} {:>2} {:>2} {:>3} {:>2} | {:>+4}{marker}",
            amp, b[0], b[1], b[2], b[3], l, dist
        );
    }
}

/// 210 Hz field decode: run the Rust encoder, decode its output through
/// `decode_trace`, decode the Kenwood anchor the same way, and compare
/// b0..b8 field-by-field. Skips the bit-counting layer to give actual
/// semantic comparison.
///
/// Run with `cargo test ... -- --ignored --nocapture
/// trace_210hz_b_fields`.
#[test]
#[ignore = "diagnostic; decodes 210 Hz Rust output to b0..b8 fields"]
fn trace_210hz_b_fields() -> TestResult {
    use mbelib_rs::decode_trace;

    println!();
    let pcm = synthesize_tone_pcm(210.0, 150, 16384);
    let frames = encode_to_ambe_frames(&pcm);
    if frames.is_empty() {
        println!("encoder produced no frames");
        return Ok(());
    }
    let half = frames.len() / 2;
    let mut counts: HashMap<[u8; FRAME_LEN], usize> = HashMap::new();
    for &f in frames.split_at(half).1 {
        *counts.entry(f).or_insert(0) += 1;
    }
    let (rust_dominant_raw, rust_count) = counts
        .iter()
        .max_by_key(|&(_, &n)| n)
        .ok_or("non-empty encoder output")?;

    let (rust_b, _, rust_l, _) = decode_trace(rust_dominant_raw);
    // Dewhiten the Kenwood capture before decoding (firmware applies
    // XOR 0x704F9340 cycled on the wire). Without this, ECC sees
    // garbage and decode_trace produces erasure/silence values.
    let anchor = anchor_for(210.0).ok_or("210 Hz anchor")?;
    let kenwood_dewhit = mbelib_rs::kenwood::anchors::apply_whitening(anchor.raw_dominant_frame);
    let (anchor_b, _, anchor_l, _) = decode_trace(&kenwood_dewhit);

    println!(
        "210 Hz Rust dominant raw : {} ({}/{} frames)",
        hex9(rust_dominant_raw),
        rust_count,
        frames.len() - half
    );
    println!(
        "210 Hz Kenwood raw dom.  : {}",
        hex9(&anchor.raw_dominant_frame)
    );
    println!("210 Hz Kenwood dewhit    : {}", hex9(&kenwood_dewhit));
    println!();
    println!("Decoded fields (b0..b8 + L):");
    println!("  field |  rust  | anchor | match | Δ");
    println!("  ------|--------|--------|-------|----");
    let names = [
        "b0_pitch", "b1_vuv", "b2_gain", "b3", "b4", "b5", "b6", "b7", "b8",
    ];
    for (name, (&r, &a)) in names.iter().zip(rust_b.iter().zip(anchor_b.iter())) {
        let ok = if r == a { "yes" } else { "NO " };
        let delta = i32::try_from(r).unwrap_or(0) - i32::try_from(a).unwrap_or(0);
        println!("  {name:<7}|  {r:>4}  |  {a:>4}  | {ok}   | {delta:>+4}");
    }
    println!(
        "  L     |  {rust_l:>4}  |  {anchor_l:>4}  | {}   |",
        if rust_l == anchor_l { "yes" } else { "NO " }
    );
    Ok(())
}

/// Pitch-tracker diagnostic: runs each anchor's tone PCM through the
/// front-end (`analyze_frame` + `pitch.estimate`) and prints the
/// period/f0/confidence the encoder sees. If the pitch tracker is
/// reporting wrong `period_samples` (e.g. octave-doubling), the b0
/// quantizer downstream has no chance to be right.
///
/// Run with `cargo test ... -- --ignored --nocapture
/// pitch_tracker_diagnostic`.
#[test]
#[ignore = "diagnostic; prints pitch-tracker output for each anchor"]
fn pitch_tracker_diagnostic() {
    use mbelib_rs::{EncoderBuffers, FftPlan, PitchTracker, analyze_frame};

    println!();
    println!("Pitch tracker output per anchor frequency:");
    println!("  freq | frame |  period (samp) |   f0 (Hz) | confidence");
    let target_freqs = [210.0, 440.0, 550.0, 660.0];
    for freq in target_freqs {
        let pcm_int = synthesize_tone_pcm(freq, 80, 16384);
        // Convert i16 PCM to f32 (the encoder API expects f32 in
        // `i16::MAX`-amplitude scale).
        let pcm_float: Vec<f32> = pcm_int.iter().map(|&s| f32::from(s)).collect();

        let mut bufs = EncoderBuffers::new();
        let mut plan = FftPlan::new();
        let mut fft_out = vec![realfft::num_complex::Complex::new(0.0, 0.0); 129];
        let mut tracker = PitchTracker::new();

        // Walk the frames; print every 20th to keep output compact
        for (frame_idx, chunk) in pcm_float.chunks_exact(160).enumerate() {
            analyze_frame(chunk, &mut bufs, &mut plan, &mut fft_out);
            let est = tracker.estimate(bufs.pitch_est_buf());
            if frame_idx == 0 || frame_idx == 39 || frame_idx == 79 {
                println!(
                    "  {:>4} | {:>5} | {:>14.3} | {:>9.2} | {:>10.4}",
                    freq, frame_idx, est.period_samples, est.f0_hz, est.confidence
                );
            }
        }
    }
}

/// "True" Hamming distance after applying the firmware whitening adapter.
/// Compares Rust encoder output XOR'd with the firmware whitener
/// against Kenwood's raw dominant frame, byte-for-byte. This is the
/// honest measure of how close we are to Kenwood-bit-exact in the
/// firmware's own wire format.
///
/// Run with `cargo test ... -- --ignored --nocapture
/// rust_encoder_distance_with_whitener`.
#[test]
#[ignore = "diagnostic; whitened comparison vs Kenwood raw frames"]
fn rust_encoder_distance_with_whitener() {
    use mbelib_rs::kenwood::anchors::apply_whitening;

    let mut total = 0_u32;
    println!();
    println!("Hamming distance: Rust encoder × firmware whitener vs Kenwood raw frame");
    println!("  freq | dist | rust whitened       | kenwood raw         | both decoded b0/L");
    for anchor in PITCH_ANCHORS {
        let pcm = synthesize_tone_pcm(anchor.frequency_hz, 150, 16384);
        let frames = encode_to_ambe_frames(&pcm);
        if frames.is_empty() {
            continue;
        }
        let half = frames.len() / 2;
        let mut counts: HashMap<[u8; FRAME_LEN], usize> = HashMap::new();
        for &f in frames.split_at(half).1 {
            *counts.entry(f).or_insert(0) += 1;
        }
        let Some((rust_dom, _)) = counts.iter().max_by_key(|&(_, &n)| n) else {
            continue;
        };
        let rust_whitened = apply_whitening(*rust_dom);
        let kenwood_raw = anchor.raw_dominant_frame;

        let dist = hamming_distance(rust_whitened, kenwood_raw);
        total += dist;

        let (rb, _, rl, _) = mbelib_rs::decode_trace(rust_dom);
        let kenwood_dewhit = apply_whitening(kenwood_raw);
        let (kb, _, kl, _) = mbelib_rs::decode_trace(&kenwood_dewhit);

        println!(
            "  {:>4} | {:>4} | {} | {} | rust b0={} L={}, kenwood b0={} L={}",
            anchor.frequency_hz,
            dist,
            hex9(&rust_whitened),
            hex9(&kenwood_raw),
            rb[0],
            rl,
            kb[0],
            kl,
        );
    }
    println!("  TOTAL Hamming distance (whitened domain): {total} / 288 bits");
}

/// Diagnostic companion that prints Hamming distance from each anchor
/// without asserting. Lets us track port progress over time:
/// "Kenwood-exact" means total distance == 0; lower numbers across
/// commits show the port closing in.
///
/// Run with `cargo test ... -- --ignored --nocapture rust_encoder_distance`.
#[test]
#[ignore = "diagnostic; prints baseline Hamming distances vs anchors"]
fn rust_encoder_distance_baseline() {
    let mut total = 0_u32;
    println!();
    println!("Per-anchor Hamming distance (Rust encoder vs Kenwood capture):");
    println!("  freq |  dist | rust dominant     | anchor            ");
    for anchor in PITCH_ANCHORS {
        let pcm = synthesize_tone_pcm(anchor.frequency_hz, 150, 16384);
        let frames = encode_to_ambe_frames(&pcm);
        if frames.is_empty() {
            println!("  {:>4} | (no frames produced)", anchor.frequency_hz);
            continue;
        }
        let half = frames.len() / 2;
        let mut counts: HashMap<[u8; FRAME_LEN], usize> = HashMap::new();
        for &f in frames.split_at(half).1 {
            *counts.entry(mask(f)).or_insert(0) += 1;
        }
        let Some((rust_dominant, _)) = counts.iter().max_by_key(|&(_, &n)| n) else {
            continue;
        };
        let dist = hamming_distance(*rust_dominant, anchor.frame);
        total += dist;
        println!(
            "  {:>4} |  {:>4} | {} | {}",
            anchor.frequency_hz,
            dist,
            hex9(rust_dominant),
            hex9(&anchor.frame),
        );
    }
    println!("  TOTAL across 4 anchors: {total} bits (max possible 4*58=232; target 0)");
}

// AMBE 49-data-bit codeword layout per OP25/mbelib `pack_frame`:
//   pre-interleave bits  0..22 : Golay(23,12) #0 (data 0..11, parity 12..22)
//   pre-interleave bits 23..46 : Golay(23,12) #1 (data 23..34, parity 35..46)
//   pre-interleave bits 47..71 : Hamming(15,11) + unprotected b3-b8 spectral
const fn field_of(pre: u8) -> &'static str {
    match pre {
        0..=11 => "G0_data",
        12..=22 => "G0_parity",
        23..=34 => "G1_data",
        35..=46 => "G1_parity",
        _ => "spectral", // 47..=71
    }
}

/// Attribute every wrong stable bit of `rust_dominant` (vs the anchor
/// frame) to the pre-interleave AMBE field it belongs to.
fn attribute_wrong_bits(
    rust_dominant: &[u8; FRAME_LEN],
    anchor_frame: &[u8; FRAME_LEN],
    inverse: &[u8; 72],
) -> HashMap<&'static str, u32> {
    let mut local: HashMap<&'static str, u32> = HashMap::new();
    for (byte_idx, ((&rust_byte, &anchor_byte), &volatile)) in rust_dominant
        .iter()
        .zip(anchor_frame.iter())
        .zip(VOLATILE_BIT_MASK.iter())
        .enumerate()
    {
        let xor_byte = rust_byte ^ anchor_byte;
        for bit_lsb in 0_u8..8 {
            let bit_mask = 1 << bit_lsb;
            if xor_byte & bit_mask == 0 {
                continue;
            }
            // Skip volatile bits (already not in the stable comparison).
            if volatile & bit_mask != 0 {
                continue;
            }
            // MSB-first within byte → ambe_fr index
            let ambe_fr = byte_idx * 8 + (7 - usize::from(bit_lsb));
            let pre = inverse.get(ambe_fr).copied().unwrap_or(u8::MAX);
            *local.entry(field_of(pre)).or_insert(0) += 1;
        }
    }
    local
}

/// Field-level diagnostic: traces every wrong bit back through the
/// DSD interleaver to its pre-interleave AMBE codeword position, then
/// categorizes by the field it belongs to (Golay #0 data/parity, Golay
/// #1 data/parity, unprotected b3-b8 spectral magnitudes).
///
/// Lets us answer "is the issue in pitch/voicing/gain encoding, or in
/// spectral magnitudes?" — and watch each category's error count drop
/// independently as fixes land.
///
/// Run with `cargo test ... -- --ignored --nocapture
/// rust_encoder_field_breakdown`.
#[test]
#[ignore = "diagnostic; categorizes Hamming errors by AMBE field"]
fn rust_encoder_field_breakdown() -> TestResult {
    // DSD INVERSE table: INVERSE[ambe_fr_index] = pre-interleave input bit.
    // Built once at startup from the FORWARD table that lives in the
    // private encode::interleave module — duplicated literally here so
    // this diagnostic stays decoupled from the encoder's internals.
    const FORWARD: [u8; 72] = [
        10, 22, 69, 56, 34, 46, 11, 23, 32, 44, 9, 21, 68, 55, 33, 45, 66, 53, 31, 43, 8, 20, 67,
        54, 6, 18, 65, 52, 30, 42, 7, 19, 28, 40, 5, 17, 64, 51, 29, 41, 62, 49, 27, 39, 4, 16, 63,
        50, 2, 14, 61, 48, 26, 38, 3, 15, 24, 36, 1, 13, 60, 47, 25, 37, 58, 70, 57, 35, 0, 12, 59,
        71,
    ];
    let mut inverse = [0_u8; 72];
    for (input_bit, &target) in FORWARD.iter().enumerate() {
        let slot = inverse
            .get_mut(target as usize)
            .ok_or("FORWARD entries are < 72")?;
        *slot = u8::try_from(input_bit)?;
    }

    let mut counters: HashMap<&'static str, u32> = HashMap::new();
    let mut per_anchor: Vec<(f32, HashMap<&'static str, u32>)> = Vec::new();
    println!();
    println!("Per-field bit error breakdown across the 4 anchors:");
    for anchor in PITCH_ANCHORS {
        let pcm = synthesize_tone_pcm(anchor.frequency_hz, 150, 16384);
        let frames = encode_to_ambe_frames(&pcm);
        if frames.is_empty() {
            continue;
        }
        let half = frames.len() / 2;
        let mut counts: HashMap<[u8; FRAME_LEN], usize> = HashMap::new();
        for &f in frames.split_at(half).1 {
            *counts.entry(mask(f)).or_insert(0) += 1;
        }
        let Some((rust_dominant, _)) = counts.iter().max_by_key(|&(_, &n)| n) else {
            continue;
        };

        // For each post-interleave bit position, if it's wrong AND in the
        // stable mask, attribute it to the source field.
        let local = attribute_wrong_bits(rust_dominant, &anchor.frame, &inverse);
        for (&field, &n) in &local {
            *counters.entry(field).or_insert(0) += n;
        }
        per_anchor.push((anchor.frequency_hz, local));
    }

    println!("  Per-anchor breakdown (V-path target = 210 Hz):");
    println!(
        "  freq  | G0_data | G0_par | G1_data | G1_par | spectral | total | in-OP25-pitch-range?"
    );
    for (freq, local) in &per_anchor {
        let g0d = local.get("G0_data").copied().unwrap_or(0);
        let g0p = local.get("G0_parity").copied().unwrap_or(0);
        let g1d = local.get("G1_data").copied().unwrap_or(0);
        let g1p = local.get("G1_parity").copied().unwrap_or(0);
        let sp = local.get("spectral").copied().unwrap_or(0);
        let tot = g0d + g0p + g1d + g1p + sp;
        let in_range = anchor_for(*freq).is_some_and(|a| a.in_op25_pitch_range);
        println!(
            "  {:>4}  |  {:>5}  |  {:>5} |  {:>5}  |  {:>5} |   {:>5}  |  {:>3}  | {}",
            freq,
            g0d,
            g0p,
            g1d,
            g1p,
            sp,
            tot,
            if in_range {
                "yes"
            } else {
                "no (octave-folded)"
            }
        );
    }

    let mut by_field: Vec<(&str, u32)> = counters.into_iter().collect();
    by_field.sort_by(|a, b| b.1.cmp(&a.1));
    let total: u32 = by_field.iter().map(|(_, n)| *n).sum();
    println!();
    println!("  field            | wrong bits across 4 anchors");
    println!("  -----------------|----------------------------");
    for (field, n) in &by_field {
        println!("  {field:<16} | {n:>4}");
    }
    println!("  -----------------|----------------------------");
    println!("  TOTAL            | {total}");
    println!();
    println!("  Field guide:");
    println!(
        "    G0_data    : pre-interleave bits 0-11   (Golay #0 protects pitch + voicing high)"
    );
    println!("    G0_parity  : pre-interleave bits 12-22  (auto-derived from G0_data)");
    println!("    G1_data    : pre-interleave bits 23-34  (Golay #1 protects voicing low + gain)");
    println!("    G1_parity  : pre-interleave bits 35-46  (auto-derived from G1_data)");
    println!(
        "    spectral   : pre-interleave bits 47-71  (b3-b8 spectral magnitudes, unprotected)"
    );
    Ok(())
}

fn hex9(frame: &[u8; FRAME_LEN]) -> String {
    let mut s = String::with_capacity(FRAME_LEN * 2);
    for &b in frame {
        let _ = std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}"));
    }
    s
}

fn rust_encoder_matches_one_anchor(anchor: &PitchAnchor) -> TestResult {
    // ~3 seconds of audio = 150 frames @ 50 frame/s. Anchor lock
    // typically occurs around frame 50 (encoder transient is ~50
    // frames). Take the dominant masked frame from the 2nd half.
    let pcm = synthesize_tone_pcm(anchor.frequency_hz, 150, 16384);
    let frames = encode_to_ambe_frames(&pcm);
    assert!(
        !frames.is_empty(),
        "encoder produced no frames for {} Hz",
        anchor.frequency_hz
    );

    let half = frames.len() / 2;
    let mut counts: HashMap<[u8; FRAME_LEN], usize> = HashMap::new();
    for &f in frames.split_at(half).1 {
        *counts.entry(mask(f)).or_insert(0) += 1;
    }
    let (rust_dominant, rust_count) = counts
        .iter()
        .max_by_key(|&(_, &n)| n)
        .ok_or("non-empty encoder output")?;

    let dist = hamming_distance(*rust_dominant, anchor.frame);
    assert_eq!(
        dist,
        0,
        "{} Hz: Rust encoder dominant masked frame {:02x?} differs from anchor {:02x?} by {} bits \
         ({} of {} 2nd-half frames matched)",
        anchor.frequency_hz,
        rust_dominant,
        anchor.frame,
        dist,
        rust_count,
        frames.len() - half
    );
    Ok(())
}
