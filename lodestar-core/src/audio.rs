// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! On-device RX audio: AMBE decode with loss concealment, reorder
//! rejection, causal waveform enhancement, and click-free stream
//! edges for reflector monitoring.
//!
//! Swift feeds each `ReflectorEvent::VoiceFrame`'s 12-byte payload in
//! and plays the returned 8 kHz mono PCM through `AVAudioEngine`. The
//! pipeline runs every decoded or concealed frame through mbelib-rs's
//! live enhancer, then holds one enhanced frame back so `end_stream`
//! can fade the tail.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use mbelib_rs::AmbeDecoder;
use mbelib_rs::enhance_live::{LiveWaveEnhancer, LiveWaveStream};

/// Samples per 20 ms AMBE frame at 8 kHz.
pub(crate) const FRAME_SAMPLES: usize = 160;
/// D-STAR superframe length; wire seq wraps mod this value.
const SUPERFRAME_LEN: u8 = 21;
/// Longest gap concealed frame-by-frame (10 frames = 200 ms). Beyond
/// this, repeating one voice posture sounds worse than a clean
/// dropout, so the stream resyncs instead.
const MAX_CONCEAL: u8 = 10;
/// Frames within this distance BEHIND the expected sequence are late
/// duplicates and are dropped, never double-played.
const REORDER_WINDOW: u8 = 3;
/// 10 ms raised-cosine fade at stream edges (80 samples at 8 kHz).
const FADE_SAMPLES: usize = 80;

/// Where an arriving frame's sequence lands relative to expectation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GapClass {
    InOrder,
    Conceal(u8),
    Dropout(u8),
    Late,
}

const fn classify_gap(expected: u8, seq: u8) -> GapClass {
    let gap = (seq + SUPERFRAME_LEN - expected) % SUPERFRAME_LEN;
    if gap == 0 {
        GapClass::InOrder
    } else if gap <= MAX_CONCEAL {
        GapClass::Conceal(gap)
    } else if gap >= SUPERFRAME_LEN - REORDER_WINDOW {
        GapClass::Late
    } else {
        GapClass::Dropout(gap)
    }
}

/// Per-stream RX statistics.
#[derive(Debug, Clone, Copy, Default, uniffi::Record)]
pub struct RxStreamStats {
    /// Frames decoded from the wire.
    pub received: u32,
    /// Frames lost to sequence gaps (concealed or skipped).
    pub lost: u32,
    /// Frames dropped for arriving behind the expected sequence.
    pub late: u32,
}

/// Result of finishing a stream: the faded tail plus statistics.
#[derive(Debug, Clone, uniffi::Record)]
pub struct RxStreamEnd {
    /// Final PCM: any output still inside the enhancer followed by
    /// the held-back frame with a 10 ms fade-out. Possibly empty.
    pub tail_pcm: Vec<i16>,
    /// Loss/late counters for the whole stream.
    pub stats: RxStreamStats,
}

struct PipelineInner {
    decoder: AmbeDecoder,
    enhancement: Option<LiveWaveStream>,
    enhanced: VecDeque<i16>,
    expected_seq: Option<u8>,
    stats: RxStreamStats,
    held: Option<[i16; FRAME_SAMPLES]>,
    pending_fade_in: bool,
}

impl PipelineInner {
    fn fresh(enhancement: Option<LiveWaveStream>) -> Self {
        Self {
            decoder: AmbeDecoder::new(),
            enhancement,
            enhanced: VecDeque::new(),
            expected_seq: None,
            stats: RxStreamStats {
                received: 0,
                lost: 0,
                late: 0,
            },
            held: None,
            pending_fade_in: true,
        }
    }

    /// Feed one decoded or concealed frame through the always-on live
    /// enhancer, then release every complete enhanced frame it makes
    /// ready. When the embedded model could not be loaded at startup,
    /// preserve audio with the same raw fallback Sextant uses.
    fn enhance(&mut self, pcm: &[i16; FRAME_SAMPLES], out: &mut Vec<i16>) {
        let Some(stream) = self.enhancement.as_mut() else {
            self.release(pcm, out);
            return;
        };
        let mut samples = [0.0_f32; FRAME_SAMPLES];
        for (slot, &sample) in samples.iter_mut().zip(pcm.iter()) {
            *slot = sample_to_f32(sample);
        }
        let ready = stream.push_samples_f32(&samples);
        self.append_enhanced(&ready);
        self.drain_enhanced(out);
    }

    /// Flush the live enhancer's residual lookahead and release every
    /// complete frame it was still carrying.
    fn finish_enhancement(&mut self, out: &mut Vec<i16>) {
        let Some(stream) = self.enhancement.as_mut() else {
            return;
        };
        let ready = stream.finish_f32();
        self.append_enhanced(&ready);
        self.drain_enhanced(out);
    }

    /// Convert and queue newly finalized enhancer output.
    fn append_enhanced(&mut self, samples: &[f32]) {
        self.enhanced
            .extend(samples.iter().copied().map(sample_to_i16));
    }

    /// Move complete enhanced frames through the existing edge-fade
    /// holdback. The enhancer may finalize arbitrary sample counts,
    /// while the public pipeline continues returning whole 20 ms
    /// frames.
    fn drain_enhanced(&mut self, out: &mut Vec<i16>) {
        while let Some(frame) = take_frame(&mut self.enhanced) {
            self.release(&frame, out);
        }
    }

    /// Swap `fresh` into the holdback slot, releasing (and fading in,
    /// if first) whatever was held.
    fn release(&mut self, fresh: &[i16; FRAME_SAMPLES], out: &mut Vec<i16>) {
        if let Some(mut previous) = self.held.replace(*fresh) {
            if self.pending_fade_in {
                fade_in(&mut previous);
                self.pending_fade_in = false;
            }
            out.extend_from_slice(&previous);
        }
    }
}

/// Decodes one reflector voice stream at a time into 8 kHz mono PCM.
///
/// Not tied to a session: Swift calls `start_stream` on `VoiceStart`,
/// `push_voice` per `VoiceFrame`, and `end_stream` on `VoiceEnd`.
#[derive(uniffi::Object)]
pub struct RxAudioPipeline {
    enhancer: Option<LiveWaveEnhancer>,
    inner: Mutex<PipelineInner>,
}

impl std::fmt::Debug for RxAudioPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RxAudioPipeline")
            .field("enhancement_available", &self.enhancer.is_some())
            .finish_non_exhaustive()
    }
}

#[uniffi::export]
impl RxAudioPipeline {
    /// Create an idle pipeline with live waveform enhancement enabled.
    ///
    /// The embedded model is parsed once and shared by the fresh
    /// streaming session created for each transmission. A corrupt
    /// embedded model is non-fatal: it is logged and audio falls back
    /// to the base decoder.
    #[uniffi::constructor]
    #[must_use]
    pub fn new() -> Arc<Self> {
        let enhancer = match LiveWaveEnhancer::new() {
            Ok(enhancer) => Some(enhancer),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "live RX enhancer unavailable; audio will remain raw"
                );
                None
            }
        };
        let enhancement = enhancer.as_ref().map(LiveWaveEnhancer::stream);
        Arc::new(Self {
            enhancer,
            inner: Mutex::new(PipelineInner::fresh(enhancement)),
        })
    }

    /// Reset for a new stream (fresh decoder and enhancer state,
    /// zeroed counters).
    pub fn start_stream(&self) {
        let fresh = self.fresh_inner();
        *self.lock() = fresh;
    }

    /// Feed one 12-byte voice frame (9 AMBE + 3 slow-data). Returns
    /// zero or more enhanced 160-sample PCM frames ready to play:
    /// empty while the causal enhancer is filling its initial
    /// lookahead and for late duplicates; multiple frames when a gap
    /// was concealed or buffered output becomes ready.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "UniFFI FFI boundary requires owned Vec<u8> (Swift Data)"
    )]
    #[must_use]
    pub fn push_voice(&self, seq: u8, voice_bytes: Vec<u8>) -> Vec<i16> {
        let Some(ambe_slice) = voice_bytes.get(..9) else {
            return Vec::new();
        };
        let Ok(ambe) = <[u8; 9]>::try_from(ambe_slice) else {
            return Vec::new();
        };
        if voice_bytes.len() != 12 {
            return Vec::new();
        }
        // Wire seq is 0..21 on a well-formed stream, but the value
        // arrives off the network; normalize so a hostile or corrupt
        // frame can't overflow the mod-21 ring arithmetic below.
        let seq = seq % SUPERFRAME_LEN;
        let mut out = Vec::new();
        let mut inner = self.lock();
        match inner
            .expected_seq
            .map(|expected| classify_gap(expected, seq))
        {
            Some(GapClass::Late) => {
                inner.stats.late = inner.stats.late.saturating_add(1);
                return out;
            }
            Some(GapClass::Conceal(n)) => {
                inner.stats.lost = inner.stats.lost.saturating_add(u32::from(n));
                for _ in 0..n {
                    let pcm = inner.decoder.conceal_frame();
                    inner.enhance(&pcm, &mut out);
                }
            }
            Some(GapClass::Dropout(n)) => {
                inner.stats.lost = inner.stats.lost.saturating_add(u32::from(n));
            }
            Some(GapClass::InOrder) | None => {}
        }
        let pcm = inner.decoder.decode_frame(&ambe);
        inner.enhance(&pcm, &mut out);
        inner.stats.received = inner.stats.received.saturating_add(1);
        inner.expected_seq = Some((seq + 1) % SUPERFRAME_LEN);
        out
    }

    /// Finish the stream: returns the faded tail and the stats, then
    /// resets to idle.
    #[must_use]
    pub fn end_stream(&self) -> RxStreamEnd {
        let fresh = self.fresh_inner();
        let mut inner = self.lock();
        let mut tail = Vec::new();
        inner.finish_enhancement(&mut tail);
        if let Some(mut last) = inner.held.take() {
            if inner.pending_fade_in {
                fade_in(&mut last);
            }
            fade_out(&mut last);
            tail.extend_from_slice(&last);
        }
        let stats = inner.stats;
        *inner = fresh;
        drop(inner);
        RxStreamEnd {
            tail_pcm: tail,
            stats,
        }
    }
}

impl RxAudioPipeline {
    fn fresh_inner(&self) -> PipelineInner {
        PipelineInner::fresh(self.enhancer.as_ref().map(LiveWaveEnhancer::stream))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PipelineInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Pop exactly one enhanced AMBE frame without disturbing a shorter
/// partial frame.
fn take_frame(tail: &mut VecDeque<i16>) -> Option<[i16; FRAME_SAMPLES]> {
    if tail.len() < FRAME_SAMPLES {
        return None;
    }
    let mut frame = [0_i16; FRAME_SAMPLES];
    for slot in &mut frame {
        *slot = tail.pop_front()?;
    }
    Some(frame)
}

/// Decoder-domain `i16` to the live enhancer's unit-scale input.
fn sample_to_f32(sample: i16) -> f32 {
    f32::from(sample) / 32_768.0
}

/// Live-enhancer unit-scale output back to decoder-domain `i16`.
fn sample_to_i16(sample: f32) -> i16 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the sample is clamped to the i16 range before conversion"
    )]
    let converted = (sample * 32_768.0).clamp(-32_767.0, 32_767.0) as i16;
    converted
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "gain in [0,1] scales an i16 back into i16 range; index < 80 is exact in f32"
)]
fn fade_in(pcm: &mut [i16; FRAME_SAMPLES]) {
    for (i, sample) in pcm.iter_mut().take(FADE_SAMPLES).enumerate() {
        let gain = 0.5f32.mul_add(
            -(std::f32::consts::PI * i as f32 / FADE_SAMPLES as f32).cos(),
            0.5,
        );
        *sample = (f32::from(*sample) * gain) as i16;
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "gain in [0,1] scales an i16 back into i16 range; index < 80 is exact in f32"
)]
fn fade_out(pcm: &mut [i16; FRAME_SAMPLES]) {
    let start = FRAME_SAMPLES - FADE_SAMPLES;
    for (i, sample) in pcm.iter_mut().skip(start).enumerate() {
        let gain = 0.5f32.mul_add(
            (std::f32::consts::PI * i as f32 / FADE_SAMPLES as f32).cos(),
            0.5,
        );
        *sample = (f32::from(*sample) * gain) as i16;
    }
}

#[cfg(test)]
mod tests {
    use mbelib_rs::AmbeDecoder;
    use mbelib_rs::enhance_live::LiveWaveEnhancer;

    use super::{FRAME_SAMPLES, RxAudioPipeline, fade_in, fade_out};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const TONE_FRAME: [u8; 9] = [0xD2, 0x4B, 0x28, 0xB2, 0x57, 0x44, 0xE4, 0x08, 0x1C];

    /// 12-byte frame: AMBE silence + zero slow-data.
    fn silence_frame() -> Vec<u8> {
        voice_frame(&dstar_gateway_core::AMBE_SILENCE)
    }

    fn voice_frame(ambe: &[u8; 9]) -> Vec<u8> {
        let mut frame = ambe.to_vec();
        frame.extend_from_slice(&[0u8; 3]);
        frame
    }

    fn run_tone_stream(pipeline: &RxAudioPipeline, frame_count: u8) -> Vec<i16> {
        pipeline.start_stream();
        let mut output = Vec::new();
        for seq in 0..frame_count {
            output.extend(pipeline.push_voice(seq, voice_frame(&TONE_FRAME)));
        }
        output.extend(pipeline.end_stream().tail_pcm);
        output
    }

    fn expected_enhanced_tone(frame_count: usize) -> Result<Vec<i16>, Box<dyn std::error::Error>> {
        let mut decoder = AmbeDecoder::new();
        let mut raw = Vec::with_capacity(frame_count * FRAME_SAMPLES);
        for _ in 0..frame_count {
            raw.extend_from_slice(&decoder.decode_frame(&TONE_FRAME));
        }
        let enhancer = LiveWaveEnhancer::new()?;
        let mut expected = enhancer.process(&raw);
        {
            let first: &mut [i16; FRAME_SAMPLES] = expected
                .get_mut(..FRAME_SAMPLES)
                .ok_or("missing first enhanced frame")?
                .try_into()?;
            fade_in(first);
        }
        let last_start = expected
            .len()
            .checked_sub(FRAME_SAMPLES)
            .ok_or("missing final enhanced frame")?;
        {
            let last: &mut [i16; FRAME_SAMPLES] = expected
                .get_mut(last_start..)
                .ok_or("missing final enhanced frame")?
                .try_into()?;
            fade_out(last);
        }
        Ok(expected)
    }

    #[test]
    fn in_order_frames_are_always_live_enhanced() -> TestResult {
        let p = RxAudioPipeline::new();
        p.start_stream();
        let mut pcm = Vec::new();
        for seq in 0_u8..5 {
            let ready = p.push_voice(seq, voice_frame(&TONE_FRAME));
            if seq < 3 {
                assert!(
                    ready.is_empty(),
                    "the live enhancer must retain its initial 512-sample lookahead"
                );
            }
            assert_eq!(
                ready.len() % FRAME_SAMPLES,
                0,
                "push output stays AMBE-frame aligned"
            );
            pcm.extend(ready);
        }
        let end = p.end_stream();
        pcm.extend_from_slice(&end.tail_pcm);
        assert_eq!(
            pcm.len(),
            5 * FRAME_SAMPLES,
            "stream end drains all enhanced lookahead without changing duration"
        );
        assert_eq!(pcm, expected_enhanced_tone(5)?);
        assert_eq!(end.stats.received, 5);
        assert_eq!(end.stats.lost, 0);
        Ok(())
    }

    #[test]
    fn enhancer_and_decoder_reset_between_streams() {
        let p = RxAudioPipeline::new();
        let first = run_tone_stream(&p, 5);
        let second = run_tone_stream(&p, 5);
        assert_eq!(
            first, second,
            "a new transmission must not inherit recurrent or decoder state"
        );
    }

    #[test]
    fn small_gap_is_concealed() {
        let p = RxAudioPipeline::new();
        p.start_stream();
        drop(p.push_voice(0, silence_frame()));
        // seq jumps 0 → 3: two frames lost, concealed, plus the release
        // cascade: two concealed frames plus the arriving frame.
        let mut out = p.push_voice(3, silence_frame());
        let end = p.end_stream();
        out.extend_from_slice(&end.tail_pcm);
        assert_eq!(
            out.len(),
            4 * FRAME_SAMPLES,
            "received + concealed frames all survive enhancement"
        );
        assert_eq!(end.stats.lost, 2);
        assert_eq!(end.stats.received, 2);
    }

    #[test]
    fn late_frame_is_dropped() {
        let p = RxAudioPipeline::new();
        p.start_stream();
        drop(p.push_voice(0, silence_frame()));
        drop(p.push_voice(1, silence_frame()));
        assert!(
            p.push_voice(0, silence_frame()).is_empty(),
            "late duplicate never plays"
        );
        let end = p.end_stream();
        assert_eq!(
            end.tail_pcm.len(),
            2 * FRAME_SAMPLES,
            "late input does not advance the enhancer"
        );
        assert_eq!(end.stats.late, 1);
    }

    #[test]
    fn long_dropout_resyncs_without_concealment() {
        let p = RxAudioPipeline::new();
        p.start_stream();
        drop(p.push_voice(0, silence_frame()));
        // Gap of 14 (> MAX_CONCEAL, < 21-REORDER_WINDOW): resync
        // without feeding concealment into the enhancer.
        let mut out = p.push_voice(15, silence_frame());
        let end = p.end_stream();
        out.extend_from_slice(&end.tail_pcm);
        assert_eq!(
            out.len(),
            2 * FRAME_SAMPLES,
            "dropout must contain only the two received frames"
        );
        assert_eq!(end.stats.lost, 14);
    }

    #[test]
    fn out_of_range_wire_seq_is_normalized_not_panicking() {
        let p = RxAudioPipeline::new();
        p.start_stream();
        drop(p.push_voice(0, silence_frame()));
        // 255 % 21 == 3: behaves exactly like a seq-3 arrival (2 concealed).
        let mut out = p.push_voice(255, silence_frame());
        let end = p.end_stream();
        out.extend_from_slice(&end.tail_pcm);
        assert_eq!(out.len(), 4 * FRAME_SAMPLES);
        assert_eq!(end.stats.lost, 2);
        assert_eq!(end.stats.received, 2);
    }

    #[test]
    fn wrong_length_frame_is_ignored() {
        let p = RxAudioPipeline::new();
        p.start_stream();
        assert!(p.push_voice(0, vec![0u8; 5]).is_empty());
        let end = p.end_stream();
        assert_eq!(end.stats.received, 0);
    }
}
