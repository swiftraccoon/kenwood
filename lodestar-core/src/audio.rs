// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! On-device RX audio: AMBE decode with loss concealment, reorder
//! rejection, and click-free stream edges for reflector monitoring.
//!
//! Swift feeds each `ReflectorEvent::VoiceFrame`'s 12-byte payload in
//! and plays the returned 8 kHz mono PCM through `AVAudioEngine`. The
//! pipeline holds one frame back so `end_stream` can fade the tail.

use std::sync::{Arc, Mutex, PoisonError};

use mbelib_rs::AmbeDecoder;

/// Samples per 20 ms AMBE frame at 8 kHz.
pub(crate) const FRAME_SAMPLES: usize = 160;
/// D-STAR superframe length — wire seq wraps mod this value.
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
    /// Final PCM (the held-back frame with a 10 ms fade-out), possibly empty.
    pub tail_pcm: Vec<i16>,
    /// Loss/late counters for the whole stream.
    pub stats: RxStreamStats,
}

#[derive(Debug)]
struct PipelineInner {
    decoder: AmbeDecoder,
    expected_seq: Option<u8>,
    stats: RxStreamStats,
    held: Option<[i16; FRAME_SAMPLES]>,
    pending_fade_in: bool,
}

impl PipelineInner {
    const fn fresh() -> Self {
        Self {
            decoder: AmbeDecoder::new(),
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
#[derive(Debug, uniffi::Object)]
pub struct RxAudioPipeline {
    inner: Mutex<PipelineInner>,
}

#[uniffi::export]
impl RxAudioPipeline {
    /// Create an idle pipeline.
    #[uniffi::constructor]
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(PipelineInner::fresh()),
        })
    }

    /// Reset for a new stream (fresh decoder state, zeroed counters).
    pub fn start_stream(&self) {
        *self.lock() = PipelineInner::fresh();
    }

    /// Feed one 12-byte voice frame (9 AMBE + 3 slow-data). Returns
    /// zero or more 160-sample PCM frames ready to play: empty for the
    /// first (held-back) frame and for late duplicates; multiple
    /// frames when a gap was concealed.
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
        // arrives off the network — normalize so a hostile or corrupt
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
                    inner.release(&pcm, &mut out);
                }
            }
            Some(GapClass::Dropout(n)) => {
                inner.stats.lost = inner.stats.lost.saturating_add(u32::from(n));
            }
            Some(GapClass::InOrder) | None => {}
        }
        let pcm = inner.decoder.decode_frame(&ambe);
        inner.release(&pcm, &mut out);
        inner.stats.received = inner.stats.received.saturating_add(1);
        inner.expected_seq = Some((seq + 1) % SUPERFRAME_LEN);
        out
    }

    /// Finish the stream: returns the faded tail and the stats, then
    /// resets to idle.
    #[must_use]
    pub fn end_stream(&self) -> RxStreamEnd {
        let mut inner = self.lock();
        let mut tail = Vec::new();
        if let Some(mut last) = inner.held.take() {
            if inner.pending_fade_in {
                fade_in(&mut last);
            }
            fade_out(&mut last);
            tail.extend_from_slice(&last);
        }
        let stats = inner.stats;
        *inner = PipelineInner::fresh();
        drop(inner);
        RxStreamEnd {
            tail_pcm: tail,
            stats,
        }
    }
}

impl RxAudioPipeline {
    fn lock(&self) -> std::sync::MutexGuard<'_, PipelineInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
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
    use super::{FRAME_SAMPLES, RxAudioPipeline};

    /// 12-byte frame: AMBE silence + zero slow-data.
    fn silence_frame() -> Vec<u8> {
        let mut v = dstar_gateway_core::AMBE_SILENCE.to_vec();
        v.extend_from_slice(&[0u8; 3]);
        v
    }

    #[test]
    fn in_order_frames_release_with_one_frame_holdback() {
        let p = RxAudioPipeline::new();
        p.start_stream();
        assert!(
            p.push_voice(0, silence_frame()).is_empty(),
            "first frame is held back"
        );
        assert_eq!(p.push_voice(1, silence_frame()).len(), FRAME_SAMPLES);
        assert_eq!(p.push_voice(2, silence_frame()).len(), FRAME_SAMPLES);
        let end = p.end_stream();
        assert_eq!(
            end.tail_pcm.len(),
            FRAME_SAMPLES,
            "held tail flushes on end"
        );
        assert_eq!(end.stats.received, 3);
        assert_eq!(end.stats.lost, 0);
    }

    #[test]
    fn small_gap_is_concealed() {
        let p = RxAudioPipeline::new();
        p.start_stream();
        drop(p.push_voice(0, silence_frame()));
        // seq jumps 0 → 3: two frames lost, concealed, plus the release
        // cascade: 2 conceal releases + 1 decode release = 3 frames out.
        let out = p.push_voice(3, silence_frame());
        assert_eq!(out.len(), 3 * FRAME_SAMPLES);
        let end = p.end_stream();
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
        assert_eq!(end.stats.late, 1);
    }

    #[test]
    fn long_dropout_resyncs_without_concealment() {
        let p = RxAudioPipeline::new();
        p.start_stream();
        drop(p.push_voice(0, silence_frame()));
        // Gap of 14 (> MAX_CONCEAL, < 21-REORDER_WINDOW): resync, one release.
        let out = p.push_voice(15, silence_frame());
        assert_eq!(
            out.len(),
            FRAME_SAMPLES,
            "dropout must not emit concealment frames"
        );
        let end = p.end_stream();
        assert_eq!(end.stats.lost, 14);
    }

    #[test]
    fn out_of_range_wire_seq_is_normalized_not_panicking() {
        let p = RxAudioPipeline::new();
        p.start_stream();
        drop(p.push_voice(0, silence_frame()));
        // 255 % 21 == 3: behaves exactly like a seq-3 arrival (2 concealed).
        let out = p.push_voice(255, silence_frame());
        assert_eq!(out.len(), 3 * FRAME_SAMPLES);
        let end = p.end_stream();
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
