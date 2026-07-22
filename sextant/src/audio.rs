// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Audio I/O: mic capture / speaker playback / AMBE codec plumbing.
//!
//! Runs on a dedicated `std::thread` so the tokio runtime never owns
//! a `cpal::Stream` (streams are `!Send` on some platforms). The
//! thread opens the default input and output devices at whatever
//! native config cpal reports, sets up lock-free ring buffers for
//! each direction, and processes audio in its main loop:
//!
//! - RX: pull incoming [`VoiceFrame`]s from the command channel,
//!   decode to 160-sample PCM, route through the two-tail
//!   raw/enhanced selector ([`RxTailRouter`]; the operator's
//!   "Enhance RX audio" toggle picks which tail feeds playback),
//!   sinc-resample to HW rate, push to the speaker ringbuffer which
//!   the cpal output callback drains.
//! - TX: while PTT is active, drain 20 ms of HW-rate mic samples,
//!   sinc-resample to 8 kHz, feed through [`AmbeEncoder`], wrap the
//!   resulting 9-byte AMBE in a [`VoiceFrame`], and push into the
//!   session command channel via [`SessionCommand::TxFrame`].
//!
//! Sample-rate conversion uses `rubato`'s windowed-sinc resampler,
//! which bandlimits as it resamples; no separate anti-alias filter
//! is needed. The input / output devices and the recording /
//! playback paths are all driven from here.

use std::collections::VecDeque;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dstar_gateway_core::dprs::{DprsReport, Latitude, Longitude, encode_dprs};
use dstar_gateway_core::slowdata::{encode_text_message, scramble};
use dstar_gateway_core::types::Callsign;
use dstar_gateway_core::voice::{DSTAR_NULL_SLOW_DATA_BYTES, DSTAR_SYNC_BYTES, VoiceFrame};
use mbelib_rs::enhance_live::{LiveWaveEnhancer, LiveWaveStream};
use mbelib_rs::{AmbeDecoder, AmbeEncoder};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use rubato::{
    Resampler as _, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use tokio::sync::mpsc as tokio_mpsc;
use tracing::{error, info, warn};

use crate::geo::TxPosition;
use crate::session::SessionCommand;

/// AMBE native sample rate.
const AMBE_SAMPLE_RATE: u32 = 8000;
/// Samples per AMBE frame (20 ms at 8 kHz).
const AMBE_FRAME_SAMPLES: usize = 160;

/// Frames of resampled RX audio accumulated before the first push to
/// the speaker ring buffer (~60 ms). The headroom rides through the
/// stream, absorbing network jitter that would otherwise underrun.
const RX_PRIME_FRAMES: usize = 3;

/// Effective priming depth when the enhanced source is selected at
/// stream start. The live enhancer releases nothing until it has
/// seen 512 input samples (≈3.2 frames), so that fill already rides
/// inside the [`LiveWaveStream`] as jitter headroom; adding one
/// primed frame on top puts the first speaker push on the same
/// clock as raw mode (3.2 internal + 1 prime ≈ raw's
/// [`RX_PRIME_FRAMES`] + the first post-prime frame). Raw mode keeps
/// [`RX_PRIME_FRAMES`] untouched.
const RX_PRIME_FRAMES_ENHANCED: usize = 1;

/// Samples of raised-cosine ramp at 8 kHz (10 ms) applied to each
/// stream's first frame (fade-in) and final frame (fade-out).
const RX_FADE_SAMPLES: usize = 80;

/// Handle the GUI (and session task) holds; forwards user intent
/// and RX frames to the audio worker thread.
///
/// Cloneable so the session task can route RX voice frames directly
/// to the worker, bypassing the egui redraw cadence, which would
/// otherwise add ~50 ms of jitter to every frame and cause the 50 fps
/// audio to arrive in bursts that underflow the speaker ring buffer.
#[derive(Debug, Clone)]
pub(crate) struct AudioHandle {
    cmd_tx: std_mpsc::Sender<AudioCommand>,
    _worker: std::sync::Arc<JoinHandle<()>>,
}

/// Commands emitted by the GUI and consumed by the audio worker.
#[derive(Debug)]
pub(crate) enum AudioCommand {
    /// Begin mic capture; the worker also wires a `StartTx` through the
    /// session command channel so the session task emits the header
    /// before the first voice frame arrives.
    StartTx {
        /// Callsign to embed in `my_call`.
        my_call: String,
    },
    /// Stop mic capture and tell the session task to emit EOT.
    StopTx,
    /// A new RX voice stream is starting. The audio worker resets the
    /// [`AmbeDecoder`] (one decoder per voice stream) so stale
    /// synthesiser state from the prior stream doesn't leak into the
    /// first frames of the new one.
    RxStart,
    /// One voice frame arrived from the reflector: decode + play.
    RxFrame(VoiceFrame),
    /// A voice frame was lost upstream (UDP sequence gap);
    /// synthesize one concealment frame so the hole plays as a
    /// parameter-repeat instead of a 20 ms silence gap.
    RxLost,
    /// The RX stream ended (EOT, inactivity, or link loss): fade
    /// out and flush the held-back tail frame, then reset playback
    /// state for the next stream.
    RxEnd,
    /// Toggle the learned RX enhancement. `true` routes decoded RX
    /// audio through the causal live waveform enhancer; `false`
    /// plays the raw decoder output. Mid-stream flips splice at a
    /// 160-sample frame boundary with a crossfade. When the enhancer
    /// model failed to parse at worker start the toggle warns and
    /// audio stays raw.
    SetRxEnhance(bool),
    /// Set the operator's slow-data text and/or GPS beacon. Either
    /// field may be `None`. Takes effect on the next TX frame.
    SetSlowData {
        /// Slow-data text message (≤20 chars; longer is truncated).
        text: Option<String>,
        /// Manual GPS position to beacon.
        gps: Option<TxPosition>,
    },
    /// Re-enumerate audio devices; the worker replies via
    /// `AudioStatus::DeviceList`.
    EnumerateDevices,
    /// Rebuild audio I/O on the named devices (`None` = host default).
    SelectDevices {
        /// Input device name.
        input: Option<String>,
        /// Output device name.
        output: Option<String>,
    },
    /// Start recording received audio to a WAV file.
    StartRecording,
    /// Stop the active recording.
    StopRecording,
    /// Play a WAV file to the speakers locally (not transmitted).
    PlayFile {
        /// Path to a WAV file.
        path: std::path::PathBuf,
    },
    /// Transmit a WAV file as an outgoing voice stream.
    TransmitFile {
        /// Path to a WAV file.
        path: std::path::PathBuf,
    },
}

/// Status pushed from the audio worker to the GUI. Drained every
/// frame by `App::drain_audio_status`.
#[derive(Debug, Clone)]
pub(crate) enum AudioStatus {
    /// Enumerated device names (input list, output list).
    DeviceList {
        /// Input device names.
        inputs: Vec<String>,
        /// Output device names.
        outputs: Vec<String>,
    },
    /// Live peak levels in `0.0..=1.0` (TX mic, RX decoded).
    Levels {
        /// TX mic peak this tick.
        tx_peak: f32,
        /// RX decoded-audio peak this tick.
        rx_peak: f32,
    },
    /// Recording started (`true`) or stopped (`false`).
    Recording(bool),
}

/// Direction of a raised-cosine amplitude ramp.
#[derive(Debug, Clone, Copy)]
enum FadeDirection {
    /// Ramp the first `RX_FADE_SAMPLES` from 0 → 1.
    In,
    /// Ramp the last `RX_FADE_SAMPLES` from 1 → 0.
    Out,
}

/// Apply a 10 ms raised-cosine ramp in place to one 8 kHz frame.
fn apply_fade(pcm: &mut [i16; AMBE_FRAME_SAMPLES], direction: FadeDirection) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "RX_FADE_SAMPLES is 80, exact in f32"
    )]
    let ramp_len = RX_FADE_SAMPLES as f32;
    for i in 0..RX_FADE_SAMPLES {
        #[expect(clippy::cast_precision_loss, reason = "i < 80, exact in f32")]
        let rising = 0.5 * (1.0 - (std::f32::consts::PI * i as f32 / ramp_len).cos());
        let (idx, gain) = match direction {
            FadeDirection::In => (i, rising),
            FadeDirection::Out => (AMBE_FRAME_SAMPLES - RX_FADE_SAMPLES + i, 1.0 - rising),
        };
        if let Some(s) = pcm.get_mut(idx) {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "gain ≤ 1.0 keeps the product inside i16 range"
            )]
            {
                *s = (f32::from(*s) * gain) as i16;
            }
        }
    }
}

/// One-frame holdback for RX playback.
///
/// The most recent decoded frame waits here and is released only
/// when its successor arrives, so end-of-stream can fade the final
/// frame's tail before it reaches the speaker. Costs one frame
/// (20 ms) of RX latency, inaudible for reflector listening.
#[derive(Debug, Default)]
struct RxPlayback {
    /// Held-back most recent frame (8 kHz, pre-resample).
    hold: Option<[i16; AMBE_FRAME_SAMPLES]>,
    /// Frames released from holdback so far this stream.
    emitted: usize,
}

impl RxPlayback {
    /// Reset for a new stream, discarding any unflushed holdback
    /// (the previous stream ended without EOT; its tail is stale).
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Feed one decoded/concealed frame; returns the previous frame,
    /// now due for emission. The stream's first frame is faded in.
    fn push(&mut self, mut pcm: [i16; AMBE_FRAME_SAMPLES]) -> Option<[i16; AMBE_FRAME_SAMPLES]> {
        if self.hold.is_none() && self.emitted == 0 {
            apply_fade(&mut pcm, FadeDirection::In);
        }
        let due = self.hold.replace(pcm);
        if due.is_some() {
            self.emitted = self.emitted.saturating_add(1);
        }
        due
    }

    /// End of stream: returns the held final frame with its tail
    /// faded out, or `None` when nothing is held.
    fn finish(&mut self) -> Option<[i16; AMBE_FRAME_SAMPLES]> {
        let mut last = self.hold.take()?;
        apply_fade(&mut last, FadeDirection::Out);
        self.emitted = self.emitted.saturating_add(1);
        Some(last)
    }
}

/// Splice `frame` onto the other source's timeline: blend the first
/// [`RX_FADE_SAMPLES`] samples from the previous source's aligned
/// candidate `prev` into `frame`, with the raised-cosine ramp
/// [`apply_fade`] uses. Sample 0 is entirely `prev` (continuous with
/// the last frame the previous source served); samples past the ramp
/// are entirely `frame` (full blend-in). Both frames cover the same
/// output-timeline positions (the router's tails are aligned), so
/// this is a source blend, not a time blend.
fn crossfade_frame(frame: &mut [i16; AMBE_FRAME_SAMPLES], prev: &[i16; AMBE_FRAME_SAMPLES]) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "RX_FADE_SAMPLES is 80, exact in f32"
    )]
    let ramp_len = RX_FADE_SAMPLES as f32;
    for i in 0..RX_FADE_SAMPLES {
        #[expect(clippy::cast_precision_loss, reason = "i < 80, exact in f32")]
        let rising = 0.5 * (1.0 - (std::f32::consts::PI * i as f32 / ramp_len).cos());
        let (Some(slot), Some(&p)) = (frame.get_mut(i), prev.get(i)) else {
            continue;
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "convex blend of two i16 samples stays inside i16 range"
        )]
        {
            *slot = f32::from(p).mul_add(1.0 - rising, f32::from(*slot) * rising) as i16;
        }
    }
}

/// Decoder-domain `i16` → unit-scale `f32`, the live enhancer's
/// input domain (matching its own conversion).
fn rx_sample_to_f32(s: i16) -> f32 {
    f32::from(s) / 32_768.0
}

/// Unit-scale `f32` → decoder-domain `i16`, matching the live
/// enhancer's own output conversion (clamped, so full-scale output
/// never wraps).
fn rx_sample_to_i16(v: f32) -> i16 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to i16 range before the cast"
    )]
    let s = (v * 32_768.0).clamp(-32_767.0, 32_767.0) as i16;
    s
}

/// Streaming enhancement engine the RX router feeds, abstracted
/// from [`LiveWaveStream`] so the router's alignment logic is
/// unit-testable without the embedded model weights.
trait EnhanceStream {
    /// Feed unit-scale samples; returns every enhanced sample that
    /// has become final, in order.
    fn push_samples(&mut self, samples: &[f32]) -> Vec<f32>;
    /// End of stream: drain the residual lookahead so total output
    /// length equals total input length.
    fn finish(&mut self) -> Vec<f32>;
}

impl EnhanceStream for LiveWaveStream {
    fn push_samples(&mut self, samples: &[f32]) -> Vec<f32> {
        self.push_samples_f32(samples)
    }

    fn finish(&mut self) -> Vec<f32> {
        self.finish_f32()
    }
}

/// Which tail of the RX router served a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RxSource {
    /// The raw decoder-output tail.
    Raw,
    /// The live-enhanced tail.
    Enhanced,
}

/// Pop exactly one 160-sample frame off the front of `tail`, or pop
/// nothing and return `None` when fewer samples are buffered.
fn take_frame(tail: &mut VecDeque<i16>) -> Option<[i16; AMBE_FRAME_SAMPLES]> {
    if tail.len() < AMBE_FRAME_SAMPLES {
        return None;
    }
    let mut frame = [0_i16; AMBE_FRAME_SAMPLES];
    for slot in &mut frame {
        *slot = tail.pop_front()?;
    }
    Some(frame)
}

/// Aligned two-tail RX router: raw decoder output and its
/// live-enhanced counterpart, kept on the same output timeline.
///
/// Every decoded (or concealed) frame is appended to the raw tail
/// and, while a live stream is installed, pushed through the
/// enhancer, whose ready samples land in the enhanced tail. Both
/// tails start at the same origin and are consumed in lockstep:
/// serving one 160-sample frame from the selected tail discards the
/// matching 160 positions from the other, so flipping the selection
/// never repeats or skips audio. The enhanced tail runs shorter than
/// the raw tail by the enhancer's lookahead (< 56 ms plus a one-time
/// 512-sample release floor); that in-flight fill is jitter headroom
/// the playout path accounts for via [`RX_PRIME_FRAMES_ENHANCED`].
///
/// The first frame served after a source flip is crossfaded with the
/// other tail's aligned candidate ([`crossfade_frame`]) so mid-over
/// toggles splice cleanly at a frame boundary.
///
/// Generic over [`EnhanceStream`] so tests can inject deterministic
/// enhanced samples; production installs a [`LiveWaveStream`].
struct RxTailRouter<S> {
    /// Unconsumed raw decoder output; its front is the consume point.
    raw: VecDeque<i16>,
    /// Unconsumed enhanced output, same origin as `raw`.
    enhanced: VecDeque<i16>,
    /// Enhanced-timeline positions consumed ahead of production
    /// (grace frames served from raw while a freshly installed
    /// stream was still short of the consume point). Newly produced
    /// enhanced samples repay this debt (discarded, having already
    /// played as raw) before landing in the tail.
    enhanced_debt: usize,
    /// Live enhancement session, `Some` while enhancement is active
    /// on the current stream.
    stream: Option<S>,
    /// Serve from the enhanced tail when it can supply a full frame.
    /// Stays `true` after [`Self::finish_stream`] so the flushed
    /// enhanced tail, not raw, drains through end-of-stream.
    enhance_selected: bool,
    /// Source of the most recently served frame; a change triggers
    /// the crossfade splice.
    last_source: Option<RxSource>,
    /// Crossfade partner captured when the enhanced source was
    /// dropped mid-stream (toggle off): the enhanced candidate for
    /// the next raw frame's positions.
    xfade_prev: Option<[i16; AMBE_FRAME_SAMPLES]>,
    /// One raw frame may be served in place of a not-yet-ready
    /// enhanced frame right after a mid-stream toggle-on; cleared
    /// once spent so the router then holds (the playout reserve
    /// keeps the speaker fed) instead of drifting the timelines
    /// permanently apart.
    grace_available: bool,
    /// True once any frame has been served this stream; arms the
    /// mid-stream grace on toggle-on. A stream-start selection needs
    /// no grace: the priming compensation covers the enhancer fill.
    served_any: bool,
}

impl<S: EnhanceStream> RxTailRouter<S> {
    /// An idle router (no stream in progress, raw passthrough).
    const fn new() -> Self {
        Self {
            raw: VecDeque::new(),
            enhanced: VecDeque::new(),
            enhanced_debt: 0,
            stream: None,
            enhance_selected: false,
            last_source: None,
            xfade_prev: None,
            grace_available: false,
            served_any: false,
        }
    }

    /// Reset for a new RX stream. `stream` is `Some` when enhancement
    /// is selected from stream start (and the model parsed).
    fn start_stream(&mut self, stream: Option<S>) {
        self.raw.clear();
        self.enhanced.clear();
        self.enhanced_debt = 0;
        self.enhance_selected = stream.is_some();
        self.stream = stream;
        self.last_source = None;
        self.xfade_prev = None;
        self.grace_available = false;
        self.served_any = false;
    }

    /// One decoded (or concealed) frame arrives: append it to the
    /// raw tail and, while enhancement is active, feed its f32 form
    /// through the live stream, landing the returned ready samples
    /// in the enhanced tail.
    fn push_frame(&mut self, pcm: &[i16; AMBE_FRAME_SAMPLES]) {
        self.raw.extend(pcm.iter().copied());
        if let Some(stream) = self.stream.as_mut() {
            let mut f32_frame = [0.0_f32; AMBE_FRAME_SAMPLES];
            for (slot, &s) in f32_frame.iter_mut().zip(pcm.iter()) {
                *slot = rx_sample_to_f32(s);
            }
            let ready = stream.push_samples(&f32_frame);
            self.append_enhanced(&ready);
        }
    }

    /// Land freshly produced enhanced samples: repay the grace debt
    /// first (those positions already played as raw), then append.
    fn append_enhanced(&mut self, samples: &[f32]) {
        for &s in samples {
            if self.enhanced_debt > 0 {
                self.enhanced_debt -= 1;
            } else {
                self.enhanced.push_back(rx_sample_to_i16(s));
            }
        }
    }

    /// Toggle ON mid-stream (or while idle): install a fresh stream,
    /// primed with the entire unconsumed raw backlog so the enhanced
    /// tail's origin matches the raw tail's current front. With a
    /// backlog at least the enhancer's lookahead deep, the enhanced
    /// tail reaches the consume point immediately; with a shallow
    /// one (the raw path drains its tail every arrival), the switch
    /// completes after the one-frame grace plus a short hold that
    /// the playout reserve covers.
    fn enhance_on(&mut self, mut stream: S) {
        self.enhanced.clear();
        self.enhanced_debt = 0;
        self.xfade_prev = None;
        let backlog: Vec<f32> = self.raw.iter().map(|&s| rx_sample_to_f32(s)).collect();
        let ready = stream.push_samples(&backlog);
        self.stream = Some(stream);
        self.enhance_selected = true;
        self.append_enhanced(&ready);
        self.grace_available = self.served_any;
    }

    /// Toggle OFF mid-stream: switch the selection back to the
    /// (always populated) raw tail and drop the stream. When the
    /// enhanced source was actually being served, its in-flight
    /// lookahead is flushed first so the next raw frame has an
    /// aligned enhanced candidate to crossfade from.
    fn enhance_off(&mut self) {
        let stream = self.stream.take();
        if self.last_source == Some(RxSource::Enhanced)
            && let Some(mut stream) = stream
        {
            let tail = stream.finish();
            self.append_enhanced(&tail);
            self.xfade_prev = take_frame(&mut self.enhanced);
        }
        self.enhance_selected = false;
        self.enhanced.clear();
        self.enhanced_debt = 0;
        self.grace_available = false;
    }

    /// End of the RX stream: flush the live stream's residual
    /// lookahead into the enhanced tail so both tails cover every
    /// input sample (total output length equals total input length),
    /// ready to drain fully.
    fn finish_stream(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            let tail = stream.finish();
            self.append_enhanced(&tail);
        }
    }

    /// Serve the next 160-sample frame from the selected tail,
    /// consuming the matching positions from the other tail, or
    /// `None` when the selected source cannot supply a full frame
    /// yet. The first frame after a source flip is crossfaded with
    /// the other tail's aligned candidate.
    fn next_frame(&mut self) -> Option<[i16; AMBE_FRAME_SAMPLES]> {
        let source = self.pick_source()?;
        let (mut frame, partner) = match source {
            RxSource::Enhanced => {
                let frame = take_frame(&mut self.enhanced)?;
                // The raw tail always covers at least the enhanced
                // tail's positions (the stream cannot out-produce
                // its input), so this both discards the consumed
                // positions and captures the aligned raw candidate.
                (frame, take_frame(&mut self.raw))
            }
            RxSource::Raw => {
                let frame = take_frame(&mut self.raw)?;
                if self.stream.is_some() {
                    // Consume the enhanced timeline in lockstep: pop
                    // what exists, book the shortfall as debt to be
                    // repaid (discarded) once the stream produces it.
                    let have = self.enhanced.len().min(AMBE_FRAME_SAMPLES);
                    for _ in 0..have {
                        let _popped = self.enhanced.pop_front();
                    }
                    self.enhanced_debt += AMBE_FRAME_SAMPLES - have;
                }
                (frame, self.xfade_prev.take())
            }
        };
        if self.last_source.is_some_and(|last| last != source)
            && let Some(prev) = partner.as_ref()
        {
            crossfade_frame(&mut frame, prev);
        }
        self.last_source = Some(source);
        self.served_any = true;
        Some(frame)
    }

    /// Decide which tail serves the next frame, or `None` to hold.
    fn pick_source(&mut self) -> Option<RxSource> {
        if self.enhance_selected {
            if self.enhanced.len() >= AMBE_FRAME_SAMPLES {
                return Some(RxSource::Enhanced);
            }
            // A freshly toggled-on stream still short of the consume
            // point: serve one raw frame rather than stall the
            // holdback, then hold until the stream catches up; the
            // playout reserve keeps the speaker fed meanwhile.
            if self.grace_available && self.raw.len() >= AMBE_FRAME_SAMPLES {
                self.grace_available = false;
                return Some(RxSource::Raw);
            }
            return None;
        }
        (self.raw.len() >= AMBE_FRAME_SAMPLES).then_some(RxSource::Raw)
    }
}

/// Cyclic slow-data fragment scheduler for the TX path.
///
/// Holds the scrambled 3-byte fragments for the operator's current
/// text message and/or GPS beacon. `pump_tx` pulls one fragment per
/// non-sync superframe slot, cycling so the message + position repeat
/// for the duration of the transmission, the cadence a receiving
/// station expects.
#[derive(Debug, Default)]
struct TxSlowData {
    /// Scrambled fragments; empty means "send null filler".
    fragments: Vec<[u8; 3]>,
    /// Next index into `fragments`.
    cursor: usize,
}

impl TxSlowData {
    /// Rebuild the fragment sequence from the operator's current text
    /// and position. `my_call` names the DPRS sentence's station.
    fn set(&mut self, text: Option<&str>, gps: Option<&TxPosition>, my_call: &str) {
        let mut fragments = Vec::new();
        if let Some(t) = text
            && !t.is_empty()
        {
            fragments.extend(encode_text_message(t));
        }
        if let Some(pos) = gps.and_then(TxPosition::validated) {
            fragments.extend(encode_gps_fragments(pos, my_call));
        }
        self.fragments = fragments;
        self.cursor = 0;
    }

    /// Next fragment for a non-sync frame, or `None` when nothing is
    /// scheduled (caller then sends the null-filler pattern).
    fn next_fragment(&mut self) -> Option<[u8; 3]> {
        let frag = self.fragments.get(self.cursor).copied()?;
        self.cursor = (self.cursor + 1) % self.fragments.len();
        Some(frag)
    }
}

/// Fragment a DPRS sentence for `pos` into scrambled `0x3X` slow-data
/// blocks (Kenwood layout: type byte + 5 payload bytes per 6-byte
/// block, two 3-byte halves). Returns an empty vec if the position or
/// callsign can't be encoded: lenient, never panics.
fn encode_gps_fragments(pos: &TxPosition, my_call: &str) -> Vec<[u8; 3]> {
    let Ok(callsign) = Callsign::try_from_str(my_call) else {
        return Vec::new();
    };
    let (Ok(latitude), Ok(longitude)) = (
        Latitude::try_new(pos.latitude),
        Longitude::try_new(pos.longitude),
    ) else {
        return Vec::new();
    };
    let report = DprsReport {
        callsign,
        latitude,
        longitude,
        symbol: pos.symbol,
        comment: if pos.comment.is_empty() {
            None
        } else {
            Some(pos.comment.clone())
        },
    };
    let mut sentence = String::new();
    if encode_dprs(&report, &mut sentence).is_err() {
        return Vec::new();
    }
    // DPRS sentences terminate with CR; the RX assembler scans for it.
    sentence.push('\r');

    let mut out = Vec::new();
    for chunk in sentence.as_bytes().chunks(5) {
        let mut block = [0u8; 6];
        block[0] = 0x30; // GPS NMEA-passthrough type nibble.
        for (slot, &b) in block.iter_mut().skip(1).zip(chunk) {
            *slot = b;
        }
        for half in block.chunks(3) {
            let mut frag = [0u8; 3];
            frag.copy_from_slice(half);
            out.push(scramble(frag));
        }
    }
    out
}

impl AudioHandle {
    /// Spawn the audio worker thread.
    ///
    /// `session_tx` is used to push `StartTx` / `TxFrame` / `EndTx`
    /// commands at the session task; those are distinct from the
    /// `AudioCommand`s the GUI sends to the worker itself.
    ///
    /// # Errors
    ///
    /// Returns the OS error if the worker thread cannot be spawned
    /// (resource exhaustion). The audio subsystem is mandatory, so the
    /// caller (`main`) treats this as fatal.
    pub(crate) fn start(
        session_tx: tokio_mpsc::Sender<SessionCommand>,
    ) -> Result<(Self, std_mpsc::Receiver<AudioStatus>), std::io::Error> {
        let (cmd_tx, cmd_rx) = std_mpsc::channel();
        let (status_tx, status_rx) = std_mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("sextant-audio".into())
            .spawn(move || run_audio_worker(cmd_rx, session_tx, status_tx))?;
        Ok((
            Self {
                cmd_tx,
                _worker: std::sync::Arc::new(worker),
            },
            status_rx,
        ))
    }

    /// Send a command to the audio worker. Drops silently if the
    /// worker has exited.
    pub(crate) fn send(&self, cmd: AudioCommand) {
        let _unused = self.cmd_tx.send(cmd);
    }
}

fn run_audio_worker(
    cmd_rx: std_mpsc::Receiver<AudioCommand>,
    session_tx: tokio_mpsc::Sender<SessionCommand>,
    status_tx: std_mpsc::Sender<AudioStatus>,
) {
    // Init failure is no longer fatal: the worker keeps running with
    // no device so the operator can pick a working one from the audio
    // panel. The error is still surfaced to the GUI.
    let audio = match AudioIo::init() {
        Ok(a) => {
            info!(
                in_rate = a.input_rate,
                in_chs = a.input_channels,
                out_rate = a.output_rate,
                out_chs = a.output_channels,
                "audio initialised"
            );
            Some(a)
        }
        Err(e) => {
            error!(error = %e, "audio init failed; TX/RX disabled until a device is selected");
            // Surface to the GUI via the session task so the user sees
            // a real error banner. `try_send` because the session task
            // may not yet be polling; the bounded channel queues it.
            if let Err(send_err) = session_tx.try_send(SessionCommand::AudioInitError(e)) {
                error!(error = %send_err, "could not surface audio init error to GUI");
            }
            None
        }
    };

    // Parse the live RX-enhancement model once: cheap, and the
    // enhancer is stateless between streams (each stream gets its own
    // `LiveWaveStream`). Failure is non-fatal: the toggle warns and
    // RX audio stays raw.
    let rx_enhancer = match LiveWaveEnhancer::new() {
        Ok(e) => Some(e),
        Err(e) => {
            warn!(
                error = %e,
                "live RX enhancer unavailable; \"Enhance RX audio\" will keep audio raw"
            );
            None
        }
    };

    let mut worker = AudioWorker {
        audio,
        // 40 ms-lookahead pitch tracker matches OP25's reference
        // (`pitch_est.cc` 2-frame DP) and recovers ~10-15 % more
        // cross-correlation on real voice vs the zero-latency
        // single-frame fallback. The latency cost is one-way,
        // hidden inside the codec (the resampler/jitter buffer
        // between mic and AMBE encoder is already several frames
        // worth), so end-users perceive no extra delay.
        encoder: AmbeEncoder::new_with_lookahead(),
        decoder: AmbeDecoder::new(),
        tx_active: false,
        mic_scratch: Vec::with_capacity(65_536),
        resampled_in: Vec::with_capacity(AMBE_FRAME_SAMPLES),
        resampled_out: Vec::with_capacity(65_536),
        cmd_rx,
        session_tx,
        status_tx,
        tx_stats: TxStats::default(),
        tx_superframe_idx: 0,
        tx_slow_data: TxSlowData::default(),
        tx_my_call: String::new(),
        tx_peak: 0.0,
        rx_peak: 0.0,
        level_tick: 0,
        recorder: None,
        tx_file: None,
        rx_playback: RxPlayback::default(),
        rx_router: RxTailRouter::new(),
        rx_enhancer,
        rx_enhance_enabled: false,
        rx_prime: Vec::with_capacity(65_536),
        rx_prime_frames: 0,
        rx_prime_target: RX_PRIME_FRAMES,
        rx_primed: false,
    };
    // Enumerate devices once at startup so the GUI's pickers populate
    // without waiting for an explicit EnumerateDevices command.
    let (inputs, outputs) = enumerate_devices();
    let _unused = worker
        .status_tx
        .send(AudioStatus::DeviceList { inputs, outputs });
    worker.run();
    info!("audio worker shutting down");
}

struct AudioWorker {
    /// `None` when no audio device is open (init failed, or a device
    /// switch is mid-flight). TX/RX silently no-op until a device
    /// is (re)selected.
    audio: Option<AudioIo>,
    encoder: AmbeEncoder,
    decoder: AmbeDecoder,
    tx_active: bool,
    mic_scratch: Vec<f32>,
    resampled_in: Vec<f32>,
    resampled_out: Vec<f32>,
    cmd_rx: std_mpsc::Receiver<AudioCommand>,
    session_tx: tokio_mpsc::Sender<SessionCommand>,
    /// Channel for pushing device lists, level meters, and recording
    /// state to the GUI.
    status_tx: std_mpsc::Sender<AudioStatus>,
    /// Mic-level statistics for the current TX session, reset on
    /// `StartTx` and logged on `StopTx`.  A summary at TX end tells
    /// us unambiguously whether the mic is producing meaningful
    /// audio (`peak_max` ≥ ~0.1, roughly -20 dBFS, typical for spoken
    /// voice) or just floor noise / zeros (common when macOS denies
    /// permission without prompting).
    tx_stats: TxStats,
    /// Frame index inside the current 21-frame superframe, cycled
    /// `0 → 20 → 0 → ...` across the TX stream. Used to pick the
    /// slow-data payload: sync pattern at frame 0, scrambled null
    /// filler otherwise. Reset on `StartTx` so the very first voice
    /// frame after the header carries the sync pattern, which is what
    /// the receiving MMDVM modem locks onto to start audio decode.
    tx_superframe_idx: u8,
    /// Operator's TX slow-data schedule (text + GPS beacon).
    tx_slow_data: TxSlowData,
    /// Operator callsign captured on `StartTx`, used for the DPRS
    /// sentence in `tx_slow_data`.
    tx_my_call: String,
    /// Rolling TX mic peak (`0.0..=1.0`) since the last level push.
    tx_peak: f32,
    /// Rolling RX decoded-audio peak since the last level push.
    rx_peak: f32,
    /// `run`-loop ticks since the last `AudioStatus::Levels` push.
    level_tick: u8,
    /// Active WAV recording writer, `Some` while recording RX audio.
    recorder: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    /// 8 kHz mono samples of a file being transmitted, `Some` while a
    /// transmit-from-file is in progress. `pump_tx` pulls frames from
    /// here instead of the mic.
    tx_file: Option<std::vec::IntoIter<f32>>,
    /// RX one-frame holdback + fade bookkeeping.
    rx_playback: RxPlayback,
    /// Two-tail raw/enhanced RX router feeding the holdback.
    rx_router: RxTailRouter<LiveWaveStream>,
    /// Parsed live-enhancement model, built once at worker start.
    /// `None` when the embedded weights failed to parse; the
    /// enhancement toggle then warns and audio stays raw.
    rx_enhancer: Option<LiveWaveEnhancer>,
    /// Operator's "Enhance RX audio" toggle, mirrored from the GUI.
    rx_enhance_enabled: bool,
    /// Resampled PCM accumulated during stream-start priming.
    rx_prime: Vec<f32>,
    /// Frames accumulated into `rx_prime` so far this stream.
    rx_prime_frames: usize,
    /// Effective priming depth for the current stream:
    /// [`RX_PRIME_FRAMES_ENHANCED`] when the enhanced source was
    /// selected at stream start, [`RX_PRIME_FRAMES`] otherwise (raw
    /// timing is untouched).
    rx_prime_target: usize,
    /// True once priming has flushed; frames then push straight
    /// through to the speaker.
    rx_primed: bool,
}

#[derive(Debug, Default)]
struct TxStats {
    frames: u32,
    peak_max: f32,
    peak_sum: f64,
    /// Count of frames whose peak was below the typical floor-noise
    /// threshold of -46 dBFS (0.005 linear).  A high ratio here
    /// with low `peak_max` is the "macOS denied mic" smoking gun.
    silent_frames: u32,
    /// True iff the first-frame peak has been reported.
    logged_first: bool,
}

impl TxStats {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn record(&mut self, peak: f32) -> FirstFrameInfo {
        self.frames = self.frames.saturating_add(1);
        if peak > self.peak_max {
            self.peak_max = peak;
        }
        self.peak_sum += f64::from(peak);
        if peak < 0.005 {
            self.silent_frames = self.silent_frames.saturating_add(1);
        }
        if self.logged_first {
            FirstFrameInfo { peak: f32::NAN }
        } else {
            self.logged_first = true;
            FirstFrameInfo { peak }
        }
    }

    fn mean_peak(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            self.peak_sum / f64::from(self.frames)
        }
    }
}

struct FirstFrameInfo {
    /// Peak of the first frame; `NaN` if not the first.
    peak: f32,
}

impl AudioWorker {
    fn run(&mut self) {
        loop {
            let mut shutdown = false;
            loop {
                match self.cmd_rx.try_recv() {
                    Ok(cmd) => self.handle_command(cmd),
                    Err(std_mpsc::TryRecvError::Empty) => break,
                    Err(std_mpsc::TryRecvError::Disconnected) => {
                        shutdown = true;
                        break;
                    }
                }
            }
            if shutdown {
                break;
            }
            self.pump_tx();
            // Push level meters to the GUI ~every 50 ms (10 ticks).
            self.level_tick += 1;
            if self.level_tick >= 10 {
                self.level_tick = 0;
                let _unused = self.status_tx.send(AudioStatus::Levels {
                    tx_peak: self.tx_peak,
                    rx_peak: self.rx_peak,
                });
                self.tx_peak = 0.0;
                self.rx_peak = 0.0;
            }
            // ~5 ms tick keeps CPU low while the 20 ms TX cadence
            // stays responsive. Output-path jitter is absorbed by
            // the speaker ringbuf (~1 s of headroom at HW rate).
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn handle_command(&mut self, cmd: AudioCommand) {
        match cmd {
            AudioCommand::StartTx { my_call } => {
                // Purge any stale mic samples so each keying starts
                // fresh. `AudioIo::drain_mic` is non-blocking.
                if let Some(audio) = self.audio.as_mut() {
                    audio.drain_mic();
                }
                self.tx_active = true;
                self.tx_stats.reset();
                // First voice frame after the header is frame 0 of the
                // superframe (sync). The receiving MMDVM modem locks
                // its slow-data scrambler/descrambler to this 21-frame
                // boundary; without sync at the right cadence the radio
                // gets the header but never enters audio decode.
                self.tx_superframe_idx = 0;
                // Match the constructor in `start_audio_worker`:
                // lookahead encoder for OP25-parity voice quality.
                self.encoder = AmbeEncoder::new_with_lookahead();
                // Remember the callsign for the DPRS slow-data sentence.
                self.tx_my_call.clone_from(&my_call);
                if let Err(e) = self.session_tx.try_send(SessionCommand::StartTx {
                    my_call: my_call.clone(),
                }) {
                    warn!(error = %e, "session StartTx enqueue failed");
                }
                tracing::info!(my_call, "TX path enabled; mic capture active");
            }
            AudioCommand::SetSlowData { text, gps } => {
                self.tx_slow_data
                    .set(text.as_deref(), gps.as_ref(), &self.tx_my_call);
            }
            AudioCommand::EnumerateDevices => {
                let (inputs, outputs) = enumerate_devices();
                let _unused = self
                    .status_tx
                    .send(AudioStatus::DeviceList { inputs, outputs });
            }
            AudioCommand::SelectDevices { input, output } => {
                self.select_devices(input.as_deref(), output.as_deref());
            }
            AudioCommand::StartRecording => self.start_recording(),
            AudioCommand::StopRecording => self.stop_recording(),
            AudioCommand::PlayFile { path } => self.play_file(&path),
            AudioCommand::TransmitFile { path } => self.transmit_file(&path),
            AudioCommand::StopTx => self.stop_tx_capture(),
            AudioCommand::RxStart => {
                tracing::info!("RX stream starting; decoder + playback reset");
                self.decoder = AmbeDecoder::new();
                self.rx_playback.reset();
                self.rx_prime.clear();
                self.rx_prime_frames = 0;
                self.rx_primed = false;
                let stream = if self.rx_enhance_enabled {
                    self.rx_enhancer.as_ref().map(LiveWaveEnhancer::stream)
                } else {
                    None
                };
                // Enhanced-from-start streams carry ~3.2 frames of
                // jitter headroom inside the enhancer (its 512-sample
                // release floor), so the downstream prime shrinks to
                // keep both modes' first speaker push on the same
                // clock.
                self.rx_prime_target = if stream.is_some() {
                    RX_PRIME_FRAMES_ENHANCED
                } else {
                    RX_PRIME_FRAMES
                };
                self.rx_router.start_stream(stream);
            }
            AudioCommand::RxEnd => self.finish_rx_stream(),
            AudioCommand::SetRxEnhance(enable) => self.set_rx_enhance(enable),
            AudioCommand::RxFrame(frame) => {
                tracing::trace!(
                    ambe = format_args!("{:02x?}", frame.ambe),
                    "RX frame received by audio worker"
                );
                // Decode the 9-byte AMBE into 160 i16 samples @ 8 kHz.
                let pcm_i16 = self.decoder.decode_frame(&frame.ambe);
                self.handle_rx_pcm(&pcm_i16);
            }
            AudioCommand::RxLost => {
                let pcm_i16 = self.decoder.conceal_frame();
                self.handle_rx_pcm(&pcm_i16);
            }
        }
    }

    /// Flip the RX-enhancement toggle, reconfiguring the router
    /// mid-stream when one is in flight. A repeat of the current
    /// state is a no-op.
    fn set_rx_enhance(&mut self, enable: bool) {
        if enable == self.rx_enhance_enabled {
            return;
        }
        self.rx_enhance_enabled = enable;
        if enable {
            let Some(enhancer) = self.rx_enhancer.as_ref() else {
                warn!(
                    "RX enhancement unavailable (model failed to parse at startup); \
                     audio stays raw"
                );
                return;
            };
            self.rx_router.enhance_on(enhancer.stream());
        } else {
            self.rx_router.enhance_off();
        }
        tracing::info!(enabled = enable, "RX enhancement toggled");
        // A toggle-off releases the raw backlog that had been riding
        // inside the enhancer's lookahead; serve it immediately so
        // the speaker reserve refills without waiting for the next
        // arrival.
        self.drain_rx_router();
    }

    /// Route one decoded (or concealed) 8 kHz frame through the
    /// two-tail router, then serve whatever the selected tail has
    /// ready into the holdback → emit path.
    fn handle_rx_pcm(&mut self, pcm_i16: &[i16; AMBE_FRAME_SAMPLES]) {
        self.rx_router.push_frame(pcm_i16);
        self.drain_rx_router();
    }

    /// Serve every frame the router has ready through the existing
    /// holdback → emit path (fades, recorder tee, resampler, and
    /// priming all unchanged).
    fn drain_rx_router(&mut self) {
        while let Some(frame) = self.rx_router.next_frame() {
            if let Some(due) = self.rx_playback.push(frame) {
                self.emit_rx_frame(&due);
            }
        }
    }

    /// Stream end: flush the enhancer's residual lookahead and drain
    /// both router tails fully (total played samples are identical
    /// in raw and enhanced modes), then flush the faded holdback
    /// tail and any un-flushed priming buffer (streams shorter than
    /// the priming depth), then reset.
    fn finish_rx_stream(&mut self) {
        self.rx_router.finish_stream();
        self.drain_rx_router();
        if let Some(last) = self.rx_playback.finish() {
            self.emit_rx_frame(&last);
        }
        self.flush_rx_prime();
        self.rx_playback.reset();
        self.rx_prime_frames = 0;
        self.rx_primed = false;
        // Frames arriving before the next RxStart fall back to raw
        // passthrough (the pre-enhancement behavior for orphan
        // frames); RxStart re-installs a stream if the toggle is on.
        self.rx_router.start_stream(None);
        self.rx_prime_target = RX_PRIME_FRAMES;
    }

    /// Emit one frame down the RX output path: recorder tee, level
    /// meter, resample, then prime-or-push to the speaker.
    fn emit_rx_frame(&mut self, pcm_i16: &[i16; AMBE_FRAME_SAMPLES]) {
        // While recording, tee the decoded 8 kHz PCM straight
        // to the WAV (the codec's native rate, no resampling).
        if let Some(writer) = self.recorder.as_mut() {
            for &s in pcm_i16 {
                let _unused = writer.write_sample(s);
            }
        }
        // Convert to f32 for resampling.
        self.resampled_in.clear();
        self.resampled_in
            .extend(pcm_i16.iter().map(|&s| f32::from(s) / 32768.0));
        // Track the RX peak for the level meter.
        let rx_pk = self
            .resampled_in
            .iter()
            .fold(0.0_f32, |acc, &s| acc.max(s.abs()));
        self.rx_peak = self.rx_peak.max(rx_pk);
        // Resample to HW output rate.
        if let Some(audio) = self.audio.as_mut() {
            self.resampled_out = audio.rx_resampler.process(&self.resampled_in);
        } else {
            return;
        }
        if self.rx_primed {
            if let Some(audio) = self.audio.as_mut() {
                audio.push_speaker(&self.resampled_out);
            }
        } else {
            // Accumulate the stream's first frames so playback opens
            // with jitter headroom instead of racing the network.
            // The depth is per stream: shallower when the enhanced
            // source was selected at stream start, because the
            // enhancer's release floor already holds ~3.2 frames.
            self.rx_prime.extend_from_slice(&self.resampled_out);
            self.rx_prime_frames = self.rx_prime_frames.saturating_add(1);
            if self.rx_prime_frames >= self.rx_prime_target {
                self.flush_rx_prime();
            }
        }
    }

    /// Push the accumulated priming buffer to the speaker and switch
    /// to pass-through.
    fn flush_rx_prime(&mut self) {
        if !self.rx_prime.is_empty()
            && let Some(audio) = self.audio.as_mut()
        {
            audio.push_speaker(&self.rx_prime);
        }
        self.rx_prime.clear();
        self.rx_primed = true;
    }

    /// Rebuild audio I/O on the named devices, surfacing any failure
    /// to the GUI. Tears the old streams down first.
    fn select_devices(&mut self, input: Option<&str>, output: Option<&str>) {
        // Some hosts allow only one stream per device; drop the old
        // `AudioIo` before opening the new one.
        self.audio = None;
        match AudioIo::init_with(input, output) {
            Ok(io) => self.audio = Some(io),
            Err(e) => {
                error!(error = %e, "device switch failed");
                if let Err(send_err) = self.session_tx.try_send(SessionCommand::AudioInitError(e)) {
                    error!(error = %send_err, "could not surface device error");
                }
            }
        }
    }

    /// Open a WAV recording, surfacing any failure to the GUI.
    fn start_recording(&mut self) {
        match open_recording_writer() {
            Ok(w) => {
                self.recorder = Some(w);
                let _unused = self.status_tx.send(AudioStatus::Recording(true));
            }
            Err(e) => {
                error!(error = %e, "could not start recording");
                if let Err(se) = self.session_tx.try_send(SessionCommand::AudioInitError(e)) {
                    error!(error = %se, "recording error not surfaced");
                }
            }
        }
    }

    /// Finalize and close the active recording, if any.
    fn stop_recording(&mut self) {
        if let Some(w) = self.recorder.take() {
            if let Err(e) = w.finalize() {
                error!(error = %e, "finalize recording");
            }
            let _unused = self.status_tx.send(AudioStatus::Recording(false));
        }
    }

    /// Stop a live mic transmission and log a one-line mic-health
    /// verdict so the operator can tell whether the mic was alive.
    fn stop_tx_capture(&mut self) {
        if !self.tx_active {
            return;
        }
        self.tx_active = false;
        let stats = &self.tx_stats;
        let mean = stats.mean_peak();
        let silence_ratio = if stats.frames == 0 {
            0.0
        } else {
            f64::from(stats.silent_frames) / f64::from(stats.frames)
        };
        let diagnosis = if stats.peak_max < 0.001 {
            "MIC LIKELY DENIED: peak never exceeded -60 dBFS; \
             cpal is receiving zeros. macOS permission not granted. \
             Run via `open target/Sextant.app` (rebuild the bundle first)."
        } else if stats.peak_max < 0.02 {
            "MIC VERY QUIET: peak never exceeded -34 dBFS. Either \
             the mic is muted, the input device is wrong, or the \
             gain is set very low. The AMBE encoder will treat \
             this as silence."
        } else if silence_ratio > 0.7 {
            "MIC MOSTLY SILENT: <30% of frames had usable audio. \
             Speak louder/closer to the mic."
        } else {
            "MIC OK: producing signal above the floor-noise threshold."
        };
        tracing::info!(
            frames = stats.frames,
            peak_max = format_args!("{:.4}", stats.peak_max),
            peak_mean = format_args!("{mean:.4}"),
            silent_frames = stats.silent_frames,
            silence_ratio = format_args!("{:.1}%", silence_ratio * 100.0),
            "TX mic-level summary: {diagnosis}"
        );
        if let Err(e) = self.session_tx.try_send(SessionCommand::EndTx) {
            warn!(error = %e, "session EndTx enqueue failed");
        }
        tracing::info!("TX path disabled; mic capture stopped");
    }

    /// Play a WAV file to the speakers locally (not transmitted).
    fn play_file(&mut self, path: &std::path::Path) {
        let Some(audio) = self.audio.as_mut() else {
            return;
        };
        match load_wav_resampled(path, audio.output_rate) {
            Ok(samples) => audio.push_speaker(&samples),
            Err(e) => {
                error!(error = %e, "play-file: load failed");
                if let Err(se) = self.session_tx.try_send(SessionCommand::AudioInitError(e)) {
                    error!(error = %se, "file error not surfaced");
                }
            }
        }
    }

    /// Begin transmitting a WAV file as an outgoing voice stream.
    /// Refused while a live-mic TX is already active.
    fn transmit_file(&mut self, path: &std::path::Path) {
        if self.tx_active {
            warn!("transmit-from-file ignored: already transmitting");
            return;
        }
        let samples = match load_wav_resampled(path, AMBE_SAMPLE_RATE) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "transmit-from-file: load failed");
                if let Err(se) = self.session_tx.try_send(SessionCommand::AudioInitError(e)) {
                    error!(error = %se, "file error not surfaced");
                }
                return;
            }
        };
        self.tx_superframe_idx = 0;
        self.encoder = AmbeEncoder::new_with_lookahead();
        self.tx_file = Some(samples.into_iter());
        self.tx_active = true;
        if let Err(e) = self.session_tx.try_send(SessionCommand::StartTx {
            my_call: self.tx_my_call.clone(),
        }) {
            warn!(error = %e, "file-transmit StartTx enqueue failed");
        }
    }

    /// Pull the next 160-sample frame from a transmit-from-file, or
    /// `None` when the file is exhausted (which ends the stream).
    fn next_file_frame(&mut self) -> Option<Vec<f32>> {
        let chunk: Vec<f32> = self
            .tx_file
            .as_mut()
            .map_or_else(Vec::new, |f| f.by_ref().take(AMBE_FRAME_SAMPLES).collect());
        if chunk.is_empty() {
            // File exhausted; end the outgoing stream.
            self.tx_file = None;
            self.tx_active = false;
            if let Err(e) = self.session_tx.try_send(SessionCommand::EndTx) {
                warn!(error = %e, "file-transmit EndTx enqueue failed");
            }
            return None;
        }
        Some(chunk)
    }

    /// Pull one 20-ms mic frame, resampled to the 8 kHz codec rate, or
    /// `None` when the mic ringbuf hasn't a full frame ready yet.
    fn next_mic_frame(&mut self) -> Option<Vec<f32>> {
        let audio = self.audio.as_mut()?;
        let hw_per_frame = hw_samples_per_frame(audio.input_rate);
        self.mic_scratch.clear();
        if !audio.pop_mic(hw_per_frame, &mut self.mic_scratch) {
            return None;
        }
        // Raw HW-rate mic peak, feeding the mic-health verdict.
        let mic_peak = self
            .mic_scratch
            .iter()
            .fold(0.0_f32, |acc, &s| acc.max(s.abs()));
        let first_frame = self.tx_stats.record(mic_peak);
        if !first_frame.peak.is_nan() {
            tracing::info!(
                first_peak = format_args!("{:.4}", first_frame.peak),
                "TX: first mic-chunk peak captured"
            );
        }
        Some(audio.tx_resampler.process(&self.mic_scratch))
    }

    /// Source 20-ms voice frames (mic or file), encode each, attach
    /// the slow-data fragment, and forward to the session task.
    fn pump_tx(&mut self) {
        if !self.tx_active {
            // Avoid letting the mic ringbuf bloat while not transmitting.
            if let Some(audio) = self.audio.as_mut() {
                audio.drain_mic();
            }
            return;
        }
        loop {
            let from_file = self.tx_file.is_some();
            let Some(mut codec_frame) = (if from_file {
                self.next_file_frame()
            } else {
                self.next_mic_frame()
            }) else {
                break;
            };

            // Pad / truncate to exactly one codec frame.
            if codec_frame.len() < AMBE_FRAME_SAMPLES {
                codec_frame.resize(AMBE_FRAME_SAMPLES, 0.0);
            } else if codec_frame.len() > AMBE_FRAME_SAMPLES {
                codec_frame.truncate(AMBE_FRAME_SAMPLES);
            }
            let frame_peak = codec_frame.iter().fold(0.0_f32, |acc, &s| acc.max(s.abs()));
            self.tx_peak = self.tx_peak.max(frame_peak);

            let ambe = self.encoder.encode_frame(&codec_frame);
            // Slow-data superframe pattern: sync at frame 0, then the
            // operator's scheduled slow-data fragment (text / GPS) at
            // frames 1-20, or the scrambled null filler otherwise.
            let slow_data = if self.tx_superframe_idx == 0 {
                DSTAR_SYNC_BYTES
            } else {
                self.tx_slow_data
                    .next_fragment()
                    .unwrap_or(DSTAR_NULL_SLOW_DATA_BYTES)
            };
            self.tx_superframe_idx = (self.tx_superframe_idx + 1) % 21;
            let frame = VoiceFrame { ambe, slow_data };
            if let Err(e) = self.session_tx.try_send(SessionCommand::TxFrame(frame)) {
                warn!(error = %e, "TxFrame enqueue dropped");
            }
        }
    }
}

/// Owns the cpal streams and ringbufs. One input (mic) + one output
/// (speaker). Mono samples on both ends; stereo HW is folded to
/// mono in the callbacks.
struct AudioIo {
    _input_stream: cpal::Stream,
    _output_stream: cpal::Stream,
    mic_rx: ringbuf::HeapCons<f32>,
    speaker_tx: ringbuf::HeapProd<f32>,
    input_rate: u32,
    input_channels: u16,
    output_rate: u32,
    output_channels: u16,
    /// HW-rate mic frame → 8 kHz codec rate.
    tx_resampler: Resampler,
    /// 8 kHz codec rate → HW-rate speaker.
    rx_resampler: Resampler,
}

/// Resample a whole buffer with a fixed-chunk sinc resampler. The
/// final chunk is zero-padded. Empty on resampler-build failure.
fn resample_buffer(samples: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    const CHUNK: usize = 1024;
    if in_rate == out_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let Some(mut rs) = Resampler::new(in_rate, out_rate, CHUNK) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for window in samples.chunks(CHUNK) {
        let mut frame = window.to_vec();
        frame.resize(CHUNK, 0.0);
        out.extend(rs.process(&frame));
    }
    out
}

/// Read a WAV file as mono `f32` resampled to `target_rate`.
///
/// 16-bit PCM only; samples that don't decode as `i16` are dropped
/// (a non-16-bit file therefore plays as silence rather than an
/// error). Multi-channel input is folded to mono.
fn load_wav_resampled(path: &std::path::Path, target_rate: u32) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("open wav: {e}"))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let raw: Vec<i16> = reader.samples::<i16>().filter_map(Result::ok).collect();
    let mono: Vec<f32> = raw
        .chunks(channels)
        .map(|frame| {
            let sum: f32 = frame.iter().map(|&s| f32::from(s) / 32768.0).sum();
            #[expect(
                clippy::cast_precision_loss,
                reason = "channel count is a tiny integer (1..=8); exact in f32."
            )]
            let n = frame.len().max(1) as f32;
            sum / n
        })
        .collect();
    Ok(resample_buffer(&mono, spec.sample_rate, target_rate))
}

/// Open a new timestamped 8 kHz mono 16-bit WAV recording writer in
/// `<config dir>/sextant/recordings/`.
fn open_recording_writer() -> Result<hound::WavWriter<std::io::BufWriter<std::fs::File>>, String> {
    let mut dir = dirs_next::config_dir().ok_or("no config dir")?;
    dir.push("sextant");
    dir.push("recordings");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create recordings dir: {e}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let path = dir.join(format!("rx-{stamp}.wav"));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: AMBE_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    hound::WavWriter::create(&path, spec).map_err(|e| format!("create wav: {e}"))
}

/// Enumerate cpal device names. Returns `(input names, output names)`.
/// Devices whose name can't be read are skipped.
fn enumerate_devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let inputs = host
        .input_devices()
        .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    let outputs = host
        .output_devices()
        .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default();
    (inputs, outputs)
}

/// Find a device by name, or the host default. `is_input` selects
/// which device list / default to consult.
fn pick_device(host: &cpal::Host, name: Option<&str>, is_input: bool) -> Option<cpal::Device> {
    if let Some(want) = name {
        let devices = if is_input {
            host.input_devices().ok()
        } else {
            host.output_devices().ok()
        };
        if let Some(found) =
            devices.and_then(|mut d| d.find(|dev| dev.name().is_ok_and(|n| n == want)))
        {
            return Some(found);
        }
    }
    if is_input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
}

impl AudioIo {
    fn init() -> Result<Self, String> {
        Self::init_with(None, None)
    }

    /// Open the named input / output devices, falling back to the host
    /// default when a name is `None` or not found.
    fn init_with(input_name: Option<&str>, output_name: Option<&str>) -> Result<Self, String> {
        let host = cpal::default_host();
        let input_device = pick_device(&host, input_name, true).ok_or("no audio input device")?;
        let output_device =
            pick_device(&host, output_name, false).ok_or("no audio output device")?;

        let input_cfg = input_device
            .default_input_config()
            .map_err(|e| format!("default input config: {e}"))?;
        let output_cfg = output_device
            .default_output_config()
            .map_err(|e| format!("default output config: {e}"))?;

        let input_channels = input_cfg.channels();
        let output_channels = output_cfg.channels();
        let input_rate = input_cfg.sample_rate().0;
        let output_rate = output_cfg.sample_rate().0;

        // ~1 s of buffer at HW rate: plenty of headroom for GC /
        // scheduler hiccups without burning memory.
        let mic_cap = input_rate as usize;
        let speaker_cap = output_rate as usize;
        let (mic_prod, mic_rx) = HeapRb::<f32>::new(mic_cap).split();
        let (speaker_tx, speaker_cons) = HeapRb::<f32>::new(speaker_cap).split();

        let input_stream = build_input_stream(&input_device, &input_cfg, input_channels, mic_prod)?;
        let output_stream =
            build_output_stream(&output_device, &output_cfg, output_channels, speaker_cons)?;

        input_stream
            .play()
            .map_err(|e| format!("start input stream: {e}"))?;
        output_stream
            .play()
            .map_err(|e| format!("start output stream: {e}"))?;

        // Per-direction sinc resamplers. The TX resampler consumes one
        // 20 ms HW-rate mic frame; the RX resampler consumes one 160-
        // sample (8 kHz) decoded voice frame.
        let tx_resampler = Resampler::new(
            input_rate,
            AMBE_SAMPLE_RATE,
            hw_samples_per_frame(input_rate),
        )
        .ok_or("build TX resampler")?;
        let rx_resampler = Resampler::new(AMBE_SAMPLE_RATE, output_rate, AMBE_FRAME_SAMPLES)
            .ok_or("build RX resampler")?;

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            mic_rx,
            speaker_tx,
            input_rate,
            input_channels,
            output_rate,
            output_channels,
            tx_resampler,
            rx_resampler,
        })
    }

    fn pop_mic(&mut self, n: usize, out: &mut Vec<f32>) -> bool {
        if self.mic_rx.occupied_len() < n {
            return false;
        }
        out.reserve(n);
        for _ in 0..n {
            match self.mic_rx.try_pop() {
                Some(s) => out.push(s),
                None => return false,
            }
        }
        true
    }

    fn drain_mic(&mut self) {
        while self.mic_rx.try_pop().is_some() {}
    }

    fn push_speaker(&mut self, samples: &[f32]) {
        for &s in samples {
            if self.speaker_tx.try_push(s).is_err() {
                // Speaker ringbuf full: drop the rest. Implies
                // output is back-pressuring (shouldn't happen with
                // 1s of buffer unless the speaker stream stalled).
                return;
            }
        }
    }
}

/// Sinc resampler for one mono direction, wrapping `rubato::SincFixedIn`.
///
/// `SincFixedIn` consumes a fixed input chunk and bandlimits with its
/// own windowed-sinc filter, so it both resamples and anti-aliases in
/// one step, replacing the previous hand-rolled linear-interpolation
/// and FIR cascade. Both sextant directions resample fixed-size
/// frames (160 samples at 8 kHz on RX, one HW-rate frame on TX),
/// which is exactly the fixed-input contract `SincFixedIn` wants.
struct Resampler {
    inner: SincFixedIn<f32>,
}

impl Resampler {
    /// Build a resampler from `in_rate` to `out_rate` for fixed input
    /// chunks of `chunk` samples. `None` if rubato rejects the params.
    fn new(in_rate: u32, out_rate: u32, chunk: usize) -> Option<Self> {
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let ratio = f64::from(out_rate) / f64::from(in_rate);
        let inner = SincFixedIn::new(ratio, 1.0, params, chunk, 1).ok()?;
        Some(Self { inner })
    }

    /// Resample one fixed-size chunk. `input.len()` must equal the
    /// `chunk` the resampler was built with; a mismatch yields an
    /// empty result rather than a panic.
    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.inner.process(&[input], None).map_or_else(
            |_| Vec::new(),
            |out| out.into_iter().next().unwrap_or_default(),
        )
    }
}

/// HW samples per 20 ms at the given rate.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rate is a u32 audio sample rate (typically 8000..=192000); multiplied \
              by 0.020 and rounded the result is a small non-negative integer that \
              fits a usize on every platform we target."
)]
fn hw_samples_per_frame(rate: u32) -> usize {
    (f64::from(rate) * 0.020).round() as usize
}

fn build_input_stream(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    channels: u16,
    mut mic_prod: ringbuf::HeapProd<f32>,
) -> Result<cpal::Stream, String> {
    let err_fn = |e| error!(error = %e, "cpal input callback error");
    let stream_cfg = cfg.config();
    match cfg.sample_format() {
        SampleFormat::F32 => device
            .build_input_stream(
                &stream_cfg,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    write_mono(data, channels, &mut mic_prod);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build input stream (f32): {e}")),
        SampleFormat::I16 => device
            .build_input_stream(
                &stream_cfg,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    for chunk in data.chunks(channels as usize) {
                        let sum: f32 = chunk.iter().map(|&s| f32::from(s) / 32768.0).sum();
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "chunk.len() is a small channel count (typically \
                                      1..=8); f32 represents it exactly. Division to \
                                      average channels is audio-averaging where f32 \
                                      precision is the standard."
                        )]
                        let avg = sum / chunk.len() as f32;
                        let _unused = mic_prod.try_push(avg);
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build input stream (i16): {e}")),
        SampleFormat::U16 => device
            .build_input_stream(
                &stream_cfg,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    for chunk in data.chunks(channels as usize) {
                        let sum: f32 = chunk
                            .iter()
                            .map(|&s| (f32::from(s) - 32768.0) / 32768.0)
                            .sum();
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "chunk.len() is a small channel count (typically \
                                      1..=8); f32 represents it exactly. Division to \
                                      average channels is audio-averaging where f32 \
                                      precision is the standard."
                        )]
                        let avg = sum / chunk.len() as f32;
                        let _unused = mic_prod.try_push(avg);
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build input stream (u16): {e}")),
        other => Err(format!("unsupported input sample format: {other:?}")),
    }
}

fn build_output_stream(
    device: &cpal::Device,
    cfg: &cpal::SupportedStreamConfig,
    channels: u16,
    mut speaker_cons: ringbuf::HeapCons<f32>,
) -> Result<cpal::Stream, String> {
    let err_fn = |e| error!(error = %e, "cpal output callback error");
    let stream_cfg = cfg.config();
    match cfg.sample_format() {
        SampleFormat::F32 => device
            .build_output_stream(
                &stream_cfg,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    fill_stereo_from_mono_f32(data, channels, &mut speaker_cons);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build output stream (f32): {e}")),
        SampleFormat::I16 => device
            .build_output_stream(
                &stream_cfg,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    for chunk in data.chunks_mut(channels as usize) {
                        let s = speaker_cons.try_pop().unwrap_or(0.0);
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "s is clamped to -1.0..=1.0 then multiplied by \
                                      32767.0, yielding -32767.0..=32767.0, all \
                                      representable in i16 with no truncation."
                        )]
                        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                        for slot in chunk.iter_mut() {
                            *slot = v;
                        }
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build output stream (i16): {e}")),
        SampleFormat::U16 => device
            .build_output_stream(
                &stream_cfg,
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    for chunk in data.chunks_mut(channels as usize) {
                        let s = speaker_cons.try_pop().unwrap_or(0.0);
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "s is clamped to -1.0..=1.0, so (s + 1.0) is \
                                      0.0..=2.0, times 32767.5 is 0.0..=65535.0, \
                                      non-negative and fits in u16 with no truncation."
                        )]
                        let v = ((s.clamp(-1.0, 1.0) + 1.0) * 32767.5) as u16;
                        for slot in chunk.iter_mut() {
                            *slot = v;
                        }
                    }
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("build output stream (u16): {e}")),
        other => Err(format!("unsupported output sample format: {other:?}")),
    }
}

fn write_mono(data: &[f32], channels: u16, mic_prod: &mut ringbuf::HeapProd<f32>) {
    for chunk in data.chunks(channels as usize) {
        let sum: f32 = chunk.iter().sum();
        #[expect(
            clippy::cast_precision_loss,
            reason = "chunk.len() is a small channel count (typically 1..=8); f32 \
                      represents it exactly. Division to average channels is \
                      audio-averaging where f32 precision is the standard."
        )]
        let avg = sum / chunk.len() as f32;
        let _unused = mic_prod.try_push(avg);
    }
}

fn fill_stereo_from_mono_f32(
    data: &mut [f32],
    channels: u16,
    speaker_cons: &mut ringbuf::HeapCons<f32>,
) {
    for chunk in data.chunks_mut(channels as usize) {
        let s = speaker_cons.try_pop().unwrap_or(0.0);
        for slot in chunk.iter_mut() {
            *slot = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Resampler;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// rubato's sinc resampler must not alias: a 6500 Hz tone
    /// resampled 48 kHz → 8 kHz must not produce 1500 Hz content
    /// (6500 folds to 8000 − 6500 = 1500 without an anti-alias
    /// filter). rubato's windowed-sinc kernel bandlimits before
    /// downsampling. This guards the property the old hand-rolled
    /// FIR provided; losing it brings sextant↔sextant "garble noise"
    /// back.
    #[test]
    fn resampler_does_not_alias_6500hz_tone() -> TestResult {
        let chunk = 960_usize; // one 20 ms frame at 48 kHz
        let mut rs = Resampler::new(48_000, 8_000, chunk).ok_or("build resampler")?;

        // 10 chunks of a 6500 Hz tone at 48 kHz, peak amplitude 0.5.
        let mut down: Vec<f32> = Vec::new();
        for c in 0..10_usize {
            let mut frame = Vec::with_capacity(chunk);
            for i in 0..chunk {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test: sample index bounded by 9600, exact in f32."
                )]
                let t = (c * chunk + i) as f32 / 48_000.0;
                frame.push(0.5 * (t * 2.0 * std::f32::consts::PI * 6500.0).sin());
            }
            down.extend(rs.process(&frame));
        }

        // Single-bin DFT at 1500 Hz (the would-be alias frequency).
        let mut re = 0.0_f32;
        let mut im = 0.0_f32;
        for (i, &s) in down.iter().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test: i bounded by down.len() (~1600), exact in f32."
            )]
            let t = i as f32 / 8_000.0;
            let phase = 2.0 * std::f32::consts::PI * 1500.0 * t;
            re += s * phase.cos();
            im += s * phase.sin();
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "test: down.len() small, exact in f32."
        )]
        let n_down = down.len().max(1) as f32;
        let alias_mag = re.hypot(im) / n_down;
        assert!(
            alias_mag < 0.05,
            "1500 Hz alias amplitude {alias_mag:.4} exceeds 0.05: the \
             resampler is leaking high-frequency content into the speech \
             band (sextant↔sextant garble noise)."
        );
        Ok(())
    }

    /// Full sextant↔sextant simulation: generate a voice-like signal,
    /// resample 48 → 8 kHz (TX), AMBE-encode, AMBE-decode, resample
    /// 8 → 48 kHz (RX), and verify the output is audible, not the
    /// "garble noise" / "no voice" symptom. If this passes, the codec
    /// and resampler pipeline is internally consistent; any remaining
    /// sextant↔sextant failure is in cpal I/O, the network, or
    /// operator configuration.
    #[test]
    fn end_to_end_voice_pipeline_produces_audible_output() -> TestResult {
        use super::{AMBE_FRAME_SAMPLES, AMBE_SAMPLE_RATE};
        use mbelib_rs::{AmbeDecoder, AmbeEncoder};

        let hw_rate = 48_000_u32;
        let frame_hw = 960_usize; // 20 ms at 48 kHz
        let mut tx_rs =
            Resampler::new(hw_rate, AMBE_SAMPLE_RATE, frame_hw).ok_or("build TX resampler")?;
        let mut rx_rs = Resampler::new(AMBE_SAMPLE_RATE, hw_rate, AMBE_FRAME_SAMPLES)
            .ok_or("build RX resampler")?;

        // 1. ~2 s of voice-like signal at 48 kHz: F0 ≈ 130 Hz with
        //    vibrato + 8 decaying harmonics and a syllabic envelope.
        let n_frames = 100_usize;
        let total_hw = n_frames * frame_hw;
        let mut hw_input = Vec::with_capacity(total_hw);
        for i in 0..total_hw {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test: i bounded by 96000, exact in f32."
            )]
            let t = i as f32 / hw_rate as f32;
            let f0 = 25.0_f32.mul_add((t * 2.0 * std::f32::consts::PI * 5.0).sin(), 130.0);
            let mut s = 0.0_f32;
            for (k, amp) in [1.0_f32, 0.7, 0.5, 0.4, 0.3, 0.25, 0.2, 0.15]
                .iter()
                .enumerate()
            {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test: k bounded by 8, exact in f32."
                )]
                let kf = (k + 1) as f32;
                s += amp * (t * 2.0 * std::f32::consts::PI * f0 * kf).sin();
            }
            let env = 0.3_f32.mul_add((t * 2.0 * std::f32::consts::PI * 3.0).sin().abs(), 0.4);
            hw_input.push(0.4 * env * s);
        }

        // 2. TX: resample 48 → 8 kHz, AMBE-encode.
        let mut enc = AmbeEncoder::new_with_lookahead();
        let mut wire: Vec<[u8; 9]> = Vec::with_capacity(n_frames);
        for f in 0..n_frames {
            let start = f * frame_hw;
            let chunk = hw_input.get(start..start + frame_hw).unwrap_or(&[]);
            let mut resampled = tx_rs.process(chunk);
            // Pad / truncate to exactly one codec frame.
            if resampled.len() < AMBE_FRAME_SAMPLES {
                resampled.resize(AMBE_FRAME_SAMPLES, 0.0);
            } else if resampled.len() > AMBE_FRAME_SAMPLES {
                resampled.truncate(AMBE_FRAME_SAMPLES);
            }
            wire.push(enc.encode_frame(&resampled));
        }

        // 3. RX: AMBE-decode, resample 8 → 48 kHz.
        let mut dec = AmbeDecoder::new();
        let mut hw_output: Vec<f32> = Vec::with_capacity(total_hw);
        for ambe in &wire {
            let pcm = dec.decode_frame(ambe);
            let norm: Vec<f32> = pcm.iter().map(|&s| f32::from(s) / 32768.0).collect();
            hw_output.extend(rx_rs.process(&norm));
        }

        // 4. Skip warmup: encoder, decoder, and both resamplers take
        //    a few frames to converge.
        let warmup = 8 * frame_hw;
        let body = hw_output.get(warmup..).unwrap_or(&[]);
        assert!(!body.is_empty(), "no decoded output past warmup");

        // 5. RMS / peak. The parametric codec doesn't preserve phase,
        //    but loudness should land within ~6 dB of the input.
        let sum_sq: f32 = body.iter().map(|&v| v * v).sum();
        #[expect(
            clippy::cast_precision_loss,
            reason = "test: body.len() bounded by ~96000, exact in f32 mantissa."
        )]
        let rms = (sum_sq / body.len() as f32).sqrt();
        let peak = body.iter().map(|&v| v.abs()).fold(0.0_f32, f32::max);
        assert!(
            rms > 0.02,
            "decoded RMS {rms:.4} below 0.02: pipeline producing near-silent output."
        );
        assert!(
            peak > 0.05,
            "decoded peak {peak:.4} below 0.05: pipeline producing near-silent output."
        );
        Ok(())
    }

    #[test]
    fn tx_slow_data_text_roundtrips_through_collector() {
        use dstar_gateway_core::slowdata::SlowDataTextCollector;

        let mut sched = super::TxSlowData::default();
        sched.set(Some("CQ TEST"), None, "W1AW");
        // `encode_text_message` yields exactly 8 fragments for non-empty
        // text; pull them and feed the RX collector at seq 1..=8.
        let mut collector = SlowDataTextCollector::new();
        for seq in 1u8..=8 {
            let Some(frag) = sched.next_fragment() else {
                break;
            };
            collector.push(frag, seq);
        }
        let msg = collector.take_message();
        assert!(
            matches!(&msg, Some(m) if m.starts_with(b"CQ TEST")),
            "TX-scheduled text must decode back through the RX collector, got {msg:?}"
        );
    }

    #[test]
    fn tx_slow_data_empty_yields_no_fragments() {
        let mut sched = super::TxSlowData::default();
        sched.set(None, None, "W1AW");
        assert!(sched.next_fragment().is_none());
    }

    /// Fade-in silences the very first sample and leaves everything
    /// past the ramp untouched.
    #[test]
    fn fade_in_silences_frame_start() -> TestResult {
        use super::{AMBE_FRAME_SAMPLES, FadeDirection, RX_FADE_SAMPLES, apply_fade};
        let mut pcm = [10_000_i16; AMBE_FRAME_SAMPLES];
        apply_fade(&mut pcm, FadeDirection::In);
        assert_eq!(pcm.first().copied(), Some(0), "first sample must be 0");
        let past_ramp = pcm.get(RX_FADE_SAMPLES..).ok_or("ramp within frame")?;
        assert!(
            past_ramp.iter().all(|&s| s == 10_000),
            "samples past the ramp must be untouched"
        );
        Ok(())
    }

    /// Fade-out takes the final sample to (near) zero and leaves the
    /// frame's start untouched.
    #[test]
    fn fade_out_silences_frame_end() -> TestResult {
        use super::{AMBE_FRAME_SAMPLES, FadeDirection, RX_FADE_SAMPLES, apply_fade};
        let mut pcm = [100_i16; AMBE_FRAME_SAMPLES];
        apply_fade(&mut pcm, FadeDirection::Out);
        let last = pcm.last().copied().ok_or("non-empty frame")?;
        assert!(last.abs() <= 1, "final sample must be ~0, got {last}");
        let before_ramp = pcm
            .get(..AMBE_FRAME_SAMPLES - RX_FADE_SAMPLES)
            .ok_or("ramp within frame")?;
        assert!(
            before_ramp.iter().all(|&s| s == 100),
            "samples before the ramp must be untouched"
        );
        Ok(())
    }

    /// The holdback delays every frame by exactly one slot and
    /// fades the stream's first frame in.
    #[test]
    fn holdback_delays_by_one_frame() -> TestResult {
        use super::{AMBE_FRAME_SAMPLES, RxPlayback};
        let mut rx = RxPlayback::default();
        assert!(
            rx.push([100; AMBE_FRAME_SAMPLES]).is_none(),
            "first frame is held"
        );
        let due = rx
            .push([200; AMBE_FRAME_SAMPLES])
            .ok_or("second push releases the first frame")?;
        assert_eq!(
            due.first().copied(),
            Some(0),
            "released first frame is faded in"
        );
        assert_eq!(
            due.last().copied(),
            Some(100),
            "released frame is the first one pushed"
        );
        Ok(())
    }

    /// Finish releases the held tail faded out; a second finish has
    /// nothing left.
    #[test]
    fn finish_fades_out_the_held_tail() -> TestResult {
        use super::{AMBE_FRAME_SAMPLES, RxPlayback};
        let mut rx = RxPlayback::default();
        let _held = rx.push([100; AMBE_FRAME_SAMPLES]);
        let last = rx.finish().ok_or("held frame flushed on finish")?;
        let tail = last.last().copied().ok_or("non-empty frame")?;
        assert!(tail.abs() <= 1, "tail must be faded to ~0, got {tail}");
        assert!(rx.finish().is_none(), "nothing left after finish");
        Ok(())
    }

    // --- two-tail RX router -------------------------------------

    use super::{AMBE_FRAME_SAMPLES, RX_FADE_SAMPLES, RxTailRouter, crossfade_frame};

    /// Deterministic stand-in for the live enhancer: releases
    /// nothing until `floor` input samples have arrived, then emits
    /// the input negated, lagging `lag` samples behind the newest
    /// input; `finish` drains the remainder. Negation marks a sample
    /// as having passed through "enhancement" so tests can tell the
    /// tails apart while keeping perfect timeline alignment.
    struct FakeStream {
        /// Samples that must arrive before anything is released.
        floor: usize,
        /// Samples the output lags behind the input.
        lag: usize,
        /// Every input sample seen so far.
        seen: Vec<f32>,
        /// Samples already released.
        emitted: usize,
    }

    impl FakeStream {
        const fn new(floor: usize, lag: usize) -> Self {
            Self {
                floor,
                lag,
                seen: Vec::new(),
                emitted: 0,
            }
        }

        /// Release (negated) samples `emitted..through`.
        fn release(&mut self, through: usize) -> Vec<f32> {
            let out = self
                .seen
                .get(self.emitted..through)
                .map(|s| s.iter().map(|&v| -v).collect())
                .unwrap_or_default();
            self.emitted = self.emitted.max(through);
            out
        }
    }

    impl super::EnhanceStream for FakeStream {
        fn push_samples(&mut self, samples: &[f32]) -> Vec<f32> {
            self.seen.extend_from_slice(samples);
            if self.seen.len() < self.floor {
                return Vec::new();
            }
            let through = self.seen.len().saturating_sub(self.lag);
            self.release(through)
        }

        fn finish(&mut self) -> Vec<f32> {
            let through = self.seen.len();
            self.release(through)
        }
    }

    /// A 160-sample frame with every sample set to `v`.
    const fn frame_of(v: i16) -> [i16; AMBE_FRAME_SAMPLES] {
        [v; AMBE_FRAME_SAMPLES]
    }

    /// The crossfade splice opens entirely on the previous source
    /// (boundary continuity) and lands entirely on the new source
    /// past the ramp (full blend-in), monotone in between.
    #[test]
    fn crossfade_starts_on_prev_and_fully_blends_in() -> TestResult {
        let mut frame = frame_of(-1000);
        let prev = frame_of(1000);
        crossfade_frame(&mut frame, &prev);
        assert_eq!(
            frame.first().copied(),
            Some(1000),
            "sample 0 is entirely the previous source"
        );
        let past_ramp = frame.get(RX_FADE_SAMPLES..).ok_or("ramp within frame")?;
        assert!(
            past_ramp.iter().all(|&s| s == -1000),
            "samples past the ramp are entirely the new source"
        );
        let mid = frame
            .get(RX_FADE_SAMPLES / 2)
            .copied()
            .ok_or("mid sample")?;
        assert!(
            mid.abs() <= 1,
            "ramp midpoint is an even ±1000 blend, got {mid}"
        );
        let ramp = frame.get(..RX_FADE_SAMPLES).ok_or("ramp slice")?;
        assert!(
            ramp.windows(2).all(|w| w.first() >= w.last()),
            "ramp moves monotonically from the previous source to the new one"
        );
        Ok(())
    }

    /// With no stream installed the router is a straight passthrough:
    /// every arrival is served immediately and unchanged.
    #[test]
    fn router_raw_mode_passes_frames_through_unchanged() -> TestResult {
        let mut router: RxTailRouter<FakeStream> = RxTailRouter::new();
        router.start_stream(None);
        for v in [1000_i16, 2000, 3000] {
            router.push_frame(&frame_of(v));
            let served = router.next_frame().ok_or("raw frame ready on arrival")?;
            assert!(
                served.iter().all(|&s| s == v),
                "raw passthrough must not alter samples"
            );
            assert!(router.next_frame().is_none(), "tail drains to empty");
        }
        assert!(router.raw.is_empty(), "raw tail empty after each drain");
        Ok(())
    }

    /// Enhanced-from-start: the router holds while the stream sits
    /// below its release floor (that fill is the priming
    /// compensation's headroom), then serves the enhanced form of
    /// frame 1 (un-blended, since a stream's first served frame is
    /// no source flip), with the raw tail consumed in lockstep.
    #[test]
    fn router_enhanced_from_start_holds_then_serves_aligned_enhanced() -> TestResult {
        let mut router = RxTailRouter::new();
        router.start_stream(Some(FakeStream::new(512, 447)));
        for (i, v) in [1000_i16, 2000, 3000].into_iter().enumerate() {
            router.push_frame(&frame_of(v));
            assert!(
                router.next_frame().is_none(),
                "arrival {i}: enhancer below its release floor; router holds"
            );
        }
        router.push_frame(&frame_of(4000));
        let first = router
            .next_frame()
            .ok_or("floor met, enhanced frame ready")?;
        assert!(
            first.iter().all(|&s| s == -1000),
            "served frame is enhanced frame 1, with no crossfade applied"
        );
        assert_eq!(
            router.raw.len(),
            3 * AMBE_FRAME_SAMPLES,
            "raw consumed in lockstep; three frames ride as enhancer fill"
        );
        assert!(
            router.next_frame().is_none(),
            "next frame's samples are still inside the lookahead"
        );
        Ok(())
    }

    /// Mid-stream toggle-on with a backlog at least the lookahead
    /// deep: priming the fresh stream with the entire unconsumed raw
    /// tail brings the enhanced tail to the consume point
    /// immediately, and the first enhanced frame is crossfaded from
    /// the aligned raw candidate.
    #[test]
    fn router_toggle_on_with_backlog_primes_and_crossfades_immediately() -> TestResult {
        let mut router: RxTailRouter<FakeStream> = RxTailRouter::new();
        router.start_stream(None);
        for v in [1000_i16, 2000] {
            router.push_frame(&frame_of(v));
            let served = router.next_frame().ok_or("raw serve")?;
            assert!(served.iter().all(|&s| s == v));
        }
        // Queue four frames without draining: the backlog a toggle
        // can inherit when arrivals outpace consumption.
        for v in [3000_i16, 4000, 5000, 6000] {
            router.push_frame(&frame_of(v));
        }
        router.enhance_on(FakeStream::new(512, 447));
        let spliced = router.next_frame().ok_or("enhanced ready immediately")?;
        assert_eq!(
            spliced.first().copied(),
            Some(3000),
            "splice opens fully on the previous (raw) source"
        );
        let past_ramp = spliced.get(RX_FADE_SAMPLES..).ok_or("ramp within frame")?;
        assert!(
            past_ramp.iter().all(|&s| s == -3000),
            "past the ramp the frame is fully enhanced frame 3: aligned, no skip"
        );
        assert_eq!(
            router.raw.len(),
            3 * AMBE_FRAME_SAMPLES,
            "raw consumed in lockstep with the enhanced serve"
        );
        Ok(())
    }

    /// Mid-stream toggle-on with an empty backlog (raw steady state
    /// drains its tail every arrival): one grace frame is served
    /// from raw, the router then holds while the stream's output
    /// repays the grace debt, and the switch completes at the next
    /// unplayed position: crossfaded, no repeat, no skip.
    #[test]
    fn router_toggle_on_empty_backlog_serves_one_grace_frame_then_holds() -> TestResult {
        let mut router: RxTailRouter<FakeStream> = RxTailRouter::new();
        router.start_stream(None);
        for v in [1000_i16, 2000] {
            router.push_frame(&frame_of(v));
            let _served = router.next_frame().ok_or("raw serve")?;
        }
        router.enhance_on(FakeStream::new(320, 200));
        assert!(
            router.next_frame().is_none(),
            "no backlog, nothing to serve"
        );
        router.push_frame(&frame_of(3000));
        let grace = router.next_frame().ok_or("grace frame")?;
        assert!(
            grace.iter().all(|&s| s == 3000),
            "grace frame is raw and unblended (raw → raw is not a source flip)"
        );
        assert!(
            router.next_frame().is_none(),
            "grace spent; router holds while the stream catches up"
        );
        router.push_frame(&frame_of(4000));
        assert!(
            router.next_frame().is_none(),
            "stream output repays the grace debt first, still holding"
        );
        router.push_frame(&frame_of(5000));
        assert!(
            router.next_frame().is_none(),
            "enhanced tail still short of one frame"
        );
        router.push_frame(&frame_of(6000));
        let spliced = router.next_frame().ok_or("catch-up frame")?;
        assert_eq!(
            spliced.first().copied(),
            Some(4000),
            "splice opens on the raw candidate for frame 4"
        );
        let past_ramp = spliced.get(RX_FADE_SAMPLES..).ok_or("ramp within frame")?;
        assert!(
            past_ramp.iter().all(|&s| s == -4000),
            "catch-up frame is enhanced frame 4: position continuity after grace"
        );
        Ok(())
    }

    /// Toggle OFF mid-stream: the raw tail takes over at the next
    /// unplayed position; the first raw frame is crossfaded from the
    /// enhanced candidate harvested out of the dropped stream's
    /// in-flight lookahead, and later frames are pure raw.
    #[test]
    fn router_toggle_off_crossfades_back_to_raw_without_skips() -> TestResult {
        let mut router = RxTailRouter::new();
        router.start_stream(Some(FakeStream::new(320, 200)));
        for v in [1000_i16, 2000] {
            router.push_frame(&frame_of(v));
            assert!(router.next_frame().is_none(), "stream filling");
        }
        router.push_frame(&frame_of(3000));
        let first = router.next_frame().ok_or("enhanced frame 1")?;
        assert!(first.iter().all(|&s| s == -1000), "serving enhanced");
        router.enhance_off();
        let spliced = router.next_frame().ok_or("raw resumes immediately")?;
        assert_eq!(
            spliced.first().copied(),
            Some(-2000),
            "splice opens on the previous (enhanced) source's frame 2"
        );
        let past_ramp = spliced.get(RX_FADE_SAMPLES..).ok_or("ramp within frame")?;
        assert!(
            past_ramp.iter().all(|&s| s == 2000),
            "past the ramp the frame is fully raw frame 2, no skip"
        );
        let next = router.next_frame().ok_or("raw frame 3")?;
        assert!(
            next.iter().all(|&s| s == 3000),
            "subsequent frames are pure raw (no further blending)"
        );
        Ok(())
    }

    /// The equal-total-length invariant across `RxEnd`: raw mode,
    /// enhanced-from-start, and a mid-stream toggle all play exactly
    /// the samples that arrived: no more, no less.
    #[test]
    fn router_rx_end_totals_match_across_modes() {
        fn drain(router: &mut RxTailRouter<FakeStream>) -> usize {
            let mut total = 0_usize;
            while let Some(frame) = router.next_frame() {
                total += frame.len();
            }
            total
        }

        let n_frames = 5_i16;
        let expected = 5 * AMBE_FRAME_SAMPLES;

        let mut raw_router: RxTailRouter<FakeStream> = RxTailRouter::new();
        raw_router.start_stream(None);
        let mut raw_total = 0_usize;
        for v in 0..n_frames {
            raw_router.push_frame(&frame_of(v * 1000));
            raw_total += drain(&mut raw_router);
        }
        raw_router.finish_stream();
        raw_total += drain(&mut raw_router);
        assert_eq!(raw_total, expected, "raw mode plays every arrived sample");

        let mut enh_router = RxTailRouter::new();
        enh_router.start_stream(Some(FakeStream::new(512, 447)));
        let mut enh_total = 0_usize;
        for v in 0..n_frames {
            enh_router.push_frame(&frame_of(v * 1000));
            enh_total += drain(&mut enh_router);
        }
        enh_router.finish_stream();
        enh_total += drain(&mut enh_router);
        assert_eq!(
            enh_total, expected,
            "enhanced mode flushes the lookahead at RxEnd; totals match raw"
        );

        let mut mixed: RxTailRouter<FakeStream> = RxTailRouter::new();
        mixed.start_stream(None);
        let mut mixed_total = 0_usize;
        for v in 0..2_i16 {
            mixed.push_frame(&frame_of(v * 1000));
            mixed_total += drain(&mut mixed);
        }
        mixed.enhance_on(FakeStream::new(320, 200));
        for v in 2..n_frames {
            mixed.push_frame(&frame_of(v * 1000));
            mixed_total += drain(&mut mixed);
        }
        mixed.finish_stream();
        mixed_total += drain(&mut mixed);
        assert_eq!(
            mixed_total, expected,
            "a mid-stream toggle (grace path) preserves the total"
        );
    }
}
