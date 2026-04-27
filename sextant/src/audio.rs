// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Audio I/O — mic capture / speaker playback / AMBE codec plumbing.
//!
//! Runs on a dedicated `std::thread` so the tokio runtime never owns
//! a `cpal::Stream` (streams are `!Send` on some platforms). The
//! thread opens the default input and output devices at whatever
//! native config cpal reports, sets up lock-free ring buffers for
//! each direction, and processes audio in its main loop:
//!
//! - RX: pull incoming [`VoiceFrame`]s from the command channel,
//!   decode to 160-sample PCM, linear-resample to HW rate, push to
//!   the speaker ringbuffer which the cpal output callback drains.
//! - TX: while PTT is active, drain 20 ms of HW-rate mic samples,
//!   linear-resample to 8 kHz, feed through [`AmbeEncoder`], wrap the
//!   resulting 9-byte AMBE in a [`VoiceFrame`], and push into the
//!   session command channel via [`SessionCommand::TxFrame`].
//!
//! Resampling is linear interpolation — quality is adequate for
//! speech intelligibility testing but not broadcast-quality.
//! Upgrading to `rubato` is a drop-in replacement once the
//! end-to-end path is validated.

use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dstar_gateway_core::voice::{DSTAR_SYNC_BYTES, VoiceFrame};
use mbelib_rs::{AmbeDecoder, AmbeEncoder};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use tokio::sync::mpsc as tokio_mpsc;
use tracing::{error, info, warn};

use crate::session::SessionCommand;

/// AMBE native sample rate.
const AMBE_SAMPLE_RATE: u32 = 8000;
/// Samples per AMBE frame (20 ms at 8 kHz).
const AMBE_FRAME_SAMPLES: usize = 160;

/// Handle the GUI (and session task) holds; forwards user intent
/// and RX frames to the audio worker thread.
///
/// Cloneable so the session task can route RX voice frames directly
/// to the worker — bypassing the egui redraw cadence, which would
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
    /// Begin mic capture — worker also wires a `StartTx` through the
    /// session command channel so the session task emits the header
    /// before the first voice frame arrives.
    StartTx {
        /// Callsign to embed in `my_call`.
        my_call: String,
    },
    /// Stop mic capture and tell the session task to emit EOT.
    StopTx,
    /// A new RX voice stream is starting. The audio worker resets the
    /// [`AmbeDecoder`] — one decoder per voice stream — so stale
    /// synthesiser state from the prior stream doesn't leak into the
    /// first frames of the new one.
    RxStart,
    /// One voice frame arrived from the reflector — decode + play.
    RxFrame(VoiceFrame),
}

impl AudioHandle {
    /// Spawn the audio worker thread.
    ///
    /// `session_tx` is used to push `StartTx` / `TxFrame` / `EndTx`
    /// commands at the session task; those are distinct from the
    /// `AudioCommand`s the GUI sends to the worker itself.
    pub(crate) fn start(session_tx: tokio_mpsc::Sender<SessionCommand>) -> Self {
        let (cmd_tx, cmd_rx) = std_mpsc::channel();
        #[expect(
            clippy::expect_used,
            reason = "Thread spawn can only fail from OS resource exhaustion (PTHREAD_CREATE \
                      ENOMEM/EAGAIN), which is unrecoverable inside an egui constructor — \
                      the audio subsystem is mandatory for the GUI's purpose. Panicking \
                      with a named message here produces a clearer crash report than \
                      propagating the error through the GUI init path."
        )]
        let worker = std::thread::Builder::new()
            .name("sextant-audio".into())
            .spawn(move || run_audio_worker(cmd_rx, session_tx))
            .expect("spawn audio thread");
        Self {
            cmd_tx,
            _worker: std::sync::Arc::new(worker),
        }
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
) {
    let audio = match AudioIo::init() {
        Ok(a) => a,
        Err(e) => {
            error!(error = %e, "audio init failed — TX/RX disabled");
            // Keep draining commands so the GUI doesn't deadlock on
            // a full channel.
            while let Ok(_cmd) = cmd_rx.recv() {}
            return;
        }
    };
    info!(
        in_rate = audio.input_rate,
        in_chs = audio.input_channels,
        out_rate = audio.output_rate,
        out_chs = audio.output_channels,
        "audio initialised"
    );

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
        tx_stats: TxStats::default(),
    };
    worker.run();
    info!("audio worker shutting down");
}

struct AudioWorker {
    audio: AudioIo,
    encoder: AmbeEncoder,
    decoder: AmbeDecoder,
    tx_active: bool,
    mic_scratch: Vec<f32>,
    resampled_in: Vec<f32>,
    resampled_out: Vec<f32>,
    cmd_rx: std_mpsc::Receiver<AudioCommand>,
    session_tx: tokio_mpsc::Sender<SessionCommand>,
    /// Mic-level statistics for the current TX session, reset on
    /// `StartTx` and logged on `StopTx`.  A summary at TX end tells
    /// us unambiguously whether the mic is producing meaningful
    /// audio (`peak_max` ≥ ~0.1, roughly -20 dBFS, typical for spoken
    /// voice) or just floor noise / zeros (common when macOS denies
    /// permission without prompting).
    tx_stats: TxStats,
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
                self.audio.drain_mic();
                self.tx_active = true;
                self.tx_stats.reset();
                // Match the constructor in `start_audio_worker` —
                // lookahead encoder for OP25-parity voice quality.
                self.encoder = AmbeEncoder::new_with_lookahead();
                if let Err(e) = self.session_tx.try_send(SessionCommand::StartTx {
                    my_call: my_call.clone(),
                }) {
                    warn!(error = %e, "session StartTx enqueue failed");
                }
                tracing::info!(my_call, "TX path enabled — mic capture active");
            }
            AudioCommand::StopTx => {
                if self.tx_active {
                    self.tx_active = false;
                    // Emit a clear mic-health verdict at info so the
                    // operator (and future-me reading a support log)
                    // can tell in one line whether the mic was alive
                    // this session.
                    let stats = &self.tx_stats;
                    let mean = stats.mean_peak();
                    let silence_ratio = if stats.frames == 0 {
                        0.0
                    } else {
                        f64::from(stats.silent_frames) / f64::from(stats.frames)
                    };
                    let diagnosis = if stats.peak_max < 0.001 {
                        "MIC LIKELY DENIED — peak never exceeded -60 dBFS; \
                         cpal is receiving zeros. macOS permission not granted. \
                         Run via `open target/Sextant.app` (rebuild the bundle first)."
                    } else if stats.peak_max < 0.02 {
                        "MIC VERY QUIET — peak never exceeded -34 dBFS. Either \
                         the mic is muted, the input device is wrong, or the \
                         gain is set very low. The AMBE encoder will treat \
                         this as silence."
                    } else if silence_ratio > 0.7 {
                        "MIC MOSTLY SILENT — <30% of frames had usable audio. \
                         Speak louder/closer to the mic."
                    } else {
                        "MIC OK — producing signal above the floor-noise threshold."
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
                    tracing::info!("TX path disabled — mic capture stopped");
                }
            }
            AudioCommand::RxStart => {
                tracing::info!("RX stream starting — decoder reset");
                self.decoder = AmbeDecoder::new();
            }
            AudioCommand::RxFrame(frame) => {
                tracing::trace!(
                    ambe = format_args!("{:02x?}", frame.ambe),
                    "RX frame received by audio worker"
                );
                // Decode the 9-byte AMBE into 160 i16 samples @ 8 kHz.
                let pcm_i16 = self.decoder.decode_frame(&frame.ambe);
                // Convert to f32 for resampling.
                self.resampled_in.clear();
                self.resampled_in
                    .extend(pcm_i16.iter().map(|&s| f32::from(s) / 32768.0));
                // Resample to HW output rate, push to speaker ringbuf.
                self.resampled_out.clear();
                resample_linear(
                    &self.resampled_in,
                    AMBE_SAMPLE_RATE,
                    self.audio.output_rate,
                    &mut self.resampled_out,
                );
                self.audio.push_speaker(&self.resampled_out);
            }
        }
    }

    /// Drain the mic ringbuf in 20-ms-at-8 kHz chunks; encode each;
    /// forward to the session task as `TxFrame` commands.
    fn pump_tx(&mut self) {
        if !self.tx_active {
            // Avoid letting the mic ringbuf bloat while not transmitting.
            self.audio.drain_mic();
            return;
        }
        // Samples per 20 ms at HW rate.
        let hw_per_frame = hw_samples_per_frame(self.audio.input_rate);
        loop {
            self.mic_scratch.clear();
            if !self.audio.pop_mic(hw_per_frame, &mut self.mic_scratch) {
                break;
            }
            // Diagnostic: peak mic amplitude. This is the raw
            // amplitude of the HW-rate mic samples before any
            // resampling or encoding — so it reflects what cpal is
            // actually delivering from CoreAudio.
            let peak = self
                .mic_scratch
                .iter()
                .fold(0.0_f32, |acc, &s| acc.max(s.abs()));
            let first_frame = self.tx_stats.record(peak);
            if !first_frame.peak.is_nan() {
                // Info-level so the operator sees the very first
                // frame's mic level immediately, without having to
                // wait for the TX-end summary. If this is 0.0000
                // the mic has been denied by macOS. If it's real
                // (>0.01 for spoken voice), TX is at least feeding
                // the encoder.
                tracing::info!(
                    first_peak = format_args!("{:.4}", first_frame.peak),
                    "TX: first mic-chunk peak captured"
                );
            }

            self.resampled_in.clear();
            resample_linear(
                &self.mic_scratch,
                self.audio.input_rate,
                AMBE_SAMPLE_RATE,
                &mut self.resampled_in,
            );
            // Pad / truncate to exactly AMBE_FRAME_SAMPLES — linear
            // resampling can produce N-1 or N+1 depending on rounding.
            if self.resampled_in.len() < AMBE_FRAME_SAMPLES {
                self.resampled_in.resize(AMBE_FRAME_SAMPLES, 0.0);
            } else if self.resampled_in.len() > AMBE_FRAME_SAMPLES {
                self.resampled_in.truncate(AMBE_FRAME_SAMPLES);
            }
            let ambe = self.encoder.encode_frame(&self.resampled_in);
            let frame = VoiceFrame {
                ambe,
                slow_data: DSTAR_SYNC_BYTES,
            };
            tracing::trace!(peak = format_args!("{peak:.4}"), "TX frame encoded");
            if let Err(e) = self.session_tx.try_send(SessionCommand::TxFrame(frame)) {
                warn!(error = %e, "TxFrame enqueue dropped");
            }
        }
    }
}

/// Owns the cpal streams and ringbufs. One input (mic) + one output
/// (speaker). Mono samples on both ends — stereo HW is folded to
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
}

impl AudioIo {
    fn init() -> Result<Self, String> {
        let host = cpal::default_host();
        let input_device = host
            .default_input_device()
            .ok_or("no default audio input device")?;
        let output_device = host
            .default_output_device()
            .ok_or("no default audio output device")?;

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

        // ~1 s of buffer at HW rate — plenty of headroom for GC /
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

        Ok(Self {
            _input_stream: input_stream,
            _output_stream: output_stream,
            mic_rx,
            speaker_tx,
            input_rate,
            input_channels,
            output_rate,
            output_channels,
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
                // Speaker ringbuf full — drop the rest. Implies
                // output is back-pressuring (shouldn't happen with
                // 1s of buffer unless the speaker stream stalled).
                return;
            }
        }
    }
}

/// Linear interpolation resampler. `input_rate` → `output_rate`.
///
/// Adequate for speech but not broadcast-quality. A future pass can
/// swap in `rubato::SincFixedIn` for aliasing-free conversion.
fn resample_linear(input: &[f32], input_rate: u32, output_rate: u32, output: &mut Vec<f32>) {
    if input.is_empty() || input_rate == 0 || output_rate == 0 {
        return;
    }
    if input_rate == output_rate {
        output.extend_from_slice(input);
        return;
    }
    // Anti-alias / anti-image filter at min(input, output) Nyquist:
    // linear interpolation has no built-in low-pass, so without this
    // step ALL the energy above the lower-rate Nyquist becomes
    // spectral garbage — aliased back into the band on downsampling
    // (TX path: 48 kHz mic → 8 kHz codec maps everything in
    // [4 kHz, 24 kHz] into [0, 4 kHz]) or imaged into audible
    // frequencies on upsampling (RX path: 8 kHz codec → 48 kHz speaker
    // makes the codec spectrum repeat at 8, 16, 24 kHz; the 4-12 kHz
    // image is audible as high-frequency whistle).
    //
    // **This was the root cause of the sextant↔sextant "garble noise"
    // symptom on real microphone input** — every fricative, plosive,
    // and broadband transient that the mic captured above 4 kHz
    // folded back into the speech band, replacing the formants the
    // AMBE codec was supposed to encode with phantom aliased content.
    // The encoder then dutifully encoded the alias garbage; the
    // receiver dutifully reconstructed it; users heard speech-shaped
    // noise. Synthetic-input tests didn't catch this because synthetic
    // signals are bandlimited to [0, 4 kHz] by construction.
    //
    // Cutoff: `0.45 × min_rate`. The 0.45 (vs the theoretical 0.5
    // Nyquist) gives the 24-dB/oct rolloff some margin to reach
    // significant attenuation by the time we hit the Nyquist edge.
    // Cascaded 2× biquad → 4-pole Butterworth → -24 dB/oct. At 1
    // octave above the cutoff (e.g. 7.2 kHz with a 3.6 kHz cutoff
    // for an 8 kHz target rate) we are at ~-30 dB, well below the
    // audible noise floor.
    let min_rate = input_rate.min(output_rate);
    let cutoff_hz = 0.45 * f64::from(min_rate);

    let prefiltered: Vec<f32> = if input_rate > output_rate {
        // Downsampling: filter input first to bandlimit before
        // sample-rate-reducing linear interpolation.
        let taps = design_lpf_fir(cutoff_hz, f64::from(input_rate));
        apply_fir_lpf(input, &taps)
    } else {
        // Upsampling: linear interp first (cheaper to filter at the
        // higher output rate after interpolation), filter applied
        // post-interpolation below.
        input.to_vec()
    };
    let input: &[f32] = &prefiltered;

    let ratio = f64::from(input_rate) / f64::from(output_rate);
    #[expect(
        clippy::cast_precision_loss,
        reason = "input.len() as f64 is a precision-loss-only lint; audio buffers are \
                  small (voice frames, thousands of samples max) so f64's 52-bit \
                  mantissa represents them exactly."
    )]
    let out_len_f = input.len() as f64 / ratio;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "out_len_f is .round()ed from a positive product of small usize/audio \
                  rates, always fits a usize and is non-negative."
    )]
    let out_len = out_len_f.round() as usize;
    output.reserve(out_len);
    for i in 0..out_len {
        #[expect(
            clippy::cast_precision_loss,
            reason = "i is bounded by out_len (small); i as f64 is exact for all audio \
                      buffer sizes we will ever process."
        )]
        let src_pos = i as f64 * ratio;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "src_pos is i*ratio where i>=0 and ratio>0, so src_pos is \
                      non-negative and bounded by input.len() in the typical case; the \
                      bounds check below protects against overshoot."
        )]
        let src_idx = src_pos as usize;
        if src_idx >= input.len() {
            break;
        }
        let next_idx = (src_idx + 1).min(input.len() - 1);
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            reason = "src_idx as f64 is exact for small audio indices. The difference \
                      (src_pos - src_idx) is in [0.0, 1.0); narrowing from f64 to f32 \
                      loses only fractional precision that is inaudible in linear \
                      interpolation weighting."
        )]
        let frac = (src_pos - src_idx as f64) as f32;
        let a = input.get(src_idx).copied().unwrap_or(0.0);
        let b = input.get(next_idx).copied().unwrap_or(a);
        output.push((b - a).mul_add(frac, a));
    }

    // Anti-image filter for the upsampling case: zero-stuffing-style
    // linear interpolation produces spectral images of the input at
    // every multiple of `input_rate`, of which the first image
    // (at [input_rate, output_rate - input_rate]) lands in the
    // audible 4-12 kHz band when going 8 kHz → 48 kHz. Filter the
    // output through the same Butterworth LPF used on the
    // downsampling path.
    if input_rate < output_rate && !output.is_empty() {
        let taps = design_lpf_fir(cutoff_hz, f64::from(output_rate));
        let filtered = apply_fir_lpf(output, &taps);
        output.clear();
        output.extend_from_slice(&filtered);
    }
}

/// FIR filter length for the anti-alias / anti-image low-pass.
/// 63-tap Hamming-windowed sinc gives ~53 dB stopband rejection
/// — enough to push aliased mic content (sibilance / plosives at
/// 5-8 kHz) below the audible noise floor when downsampling to the
/// 8 kHz AMBE codec rate. Linear-phase FIR avoids the
/// near-cutoff sloppy rolloff that the cascaded-biquad approach
/// suffered from (-19 dB at 8 kHz vs the -53 dB FIR delivers).
const FIR_TAPS: usize = 63;

/// Design a `FIR_TAPS`-long Hamming-windowed sinc low-pass filter
/// for the given cutoff frequency and sample rate. Returns the
/// time-domain impulse response (linear phase, symmetric).
///
/// Hamming window: `0.54 − 0.46·cos(2π·n/(N−1))`. Stopband
/// attenuation ≈ 53 dB, transition bandwidth ≈ `3.3·sample_rate/N`
/// for a 63-tap design — about 2.5 kHz at 48 kHz, narrow enough
/// that we can pass the speech band (≤3.5 kHz) cleanly while
/// rejecting alias content above 4 kHz.
fn design_lpf_fir(cutoff_hz: f64, sample_rate: f64) -> [f32; FIR_TAPS] {
    let mut taps = [0.0_f32; FIR_TAPS];
    let fc_norm = cutoff_hz / sample_rate;
    #[expect(
        clippy::cast_precision_loss,
        reason = "FIR_TAPS is a small compile-time constant; the usize→f64 cast is exact."
    )]
    let center = (FIR_TAPS - 1) as f64 / 2.0;

    let mut sum = 0.0_f64;
    for (i, slot) in taps.iter_mut().enumerate() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "i bounded by FIR_TAPS; usize→f64 exact."
        )]
        let n = i as f64 - center;
        // Sinc with normalized cutoff: 2·fc·sinc(2·fc·n).
        let arg = 2.0 * fc_norm * n;
        let sinc_val = if arg.abs() < 1e-9 {
            2.0 * fc_norm
        } else {
            (std::f64::consts::PI * arg).sin() / (std::f64::consts::PI * n)
        };
        // Hamming window.
        #[expect(
            clippy::cast_precision_loss,
            reason = "i and FIR_TAPS−1 are both small; usize→f64 exact."
        )]
        let win = (-0.46_f64).mul_add(
            (2.0 * std::f64::consts::PI * i as f64 / (FIR_TAPS - 1) as f64).cos(),
            0.54,
        );
        let h = sinc_val * win;
        sum += h;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "filter taps are bounded |h| < 1 by construction; f64→f32 narrowing \
                      drops only fractional precision below the audio noise floor."
        )]
        {
            *slot = h as f32;
        }
    }

    // Normalize for unity DC gain so the filter doesn't change the
    // signal level on the DC component (any small drift from the
    // window asymmetry is corrected here).
    if sum.abs() > 1e-12 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "normalizing factor is a finite f64 derived from a small finite \
                      sum; narrowing to f32 is safe for our coefficient magnitudes."
        )]
        let norm = (1.0 / sum) as f32;
        for slot in &mut taps {
            *slot *= norm;
        }
    }

    taps
}

/// Apply a linear-phase symmetric FIR low-pass to the input slice.
/// Implements direct convolution with zero-padding at the boundaries
/// (no reflection or wrap). Output length matches input length.
///
/// At the buffer boundaries the output is degraded by the missing
/// past-history samples, but for the sextant TX/RX path a single
/// 20 ms buffer is processed with fresh state on each call — the
/// boundary degradation is one filter-length worth of warmup
/// (≈1.3 ms at 48 kHz) which is well within the 20 ms frame.
fn apply_fir_lpf(input: &[f32], taps: &[f32; FIR_TAPS]) -> Vec<f32> {
    let mut output = Vec::with_capacity(input.len());
    let half = FIR_TAPS / 2;
    for i in 0..input.len() {
        let mut acc = 0.0_f32;
        for (k, &tap) in taps.iter().enumerate() {
            // Sample index = i + k - half. Use checked arithmetic
            // to avoid signed-cast lints. For i + k >= half the
            // sample lands inside the input; otherwise we're in the
            // pre-buffer warmup zone where the convolution sum is
            // effectively zero-padded.
            let Some(src_idx) = (i + k).checked_sub(half) else {
                continue;
            };
            if let Some(&x) = input.get(src_idx) {
                acc = tap.mul_add(x, acc);
            }
        }
        output.push(acc);
    }
    output
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
                                      32767.0, yielding -32767.0..=32767.0 — all \
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
                                      0.0..=2.0, times 32767.5 is 0.0..=65535.0 — \
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
    use super::{apply_fir_lpf, design_lpf_fir, resample_linear};

    /// FIR low-pass at 0.45 × 8000 Hz cutoff (designed for 48 kHz
    /// input rate) must reject content above 4 kHz by ≥30 dB while
    /// preserving the speech-formant range below 3 kHz to within
    /// 1 dB. This is the bare minimum to prevent the alias-folded
    /// "speech-shaped garble" symptom that broke sextant↔sextant
    /// before the FIR was added.
    #[test]
    fn fir_attenuates_above_speech_band() {
        let cutoff = 3600.0_f64;
        let fs = 48_000.0_f64;
        let taps = design_lpf_fir(cutoff, fs);

        let measure_db = |freq_hz: f64| -> f64 {
            let n_samples = 4800_usize;
            let mut sig = Vec::with_capacity(n_samples);
            for i in 0..n_samples {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test: i bounded by n_samples (4800), exact in f64."
                )]
                let t = i as f64 / fs;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "test: sine value bounded by 1.0, narrowing to f32 is exact."
                )]
                {
                    sig.push((2.0 * std::f64::consts::PI * freq_hz * t).sin() as f32);
                }
            }
            let filtered = apply_fir_lpf(&sig, &taps);
            // Use RMS energy — peak detection is sensitive to phase/edge effects.
            // Compute RMS over the steady-state region (skip filter group delay).
            let warmup = 200_usize;
            let body: Vec<f32> = filtered.into_iter().skip(warmup).collect();
            let sum_sq: f32 = body.iter().map(|&v| v * v).sum();
            #[expect(
                clippy::cast_precision_loss,
                reason = "test: body.len() bounded by 4800, exact in f32 mantissa."
            )]
            let n = body.len() as f32;
            let rms = (sum_sq / n).sqrt();
            // Compare against input RMS (which is 1/√2 for unit-amplitude sine).
            let in_rms = 1.0_f32 / std::f32::consts::SQRT_2;
            20.0 * f64::from((rms / in_rms).max(1e-9_f32)).log10()
        };

        // Passband — should be essentially unattenuated.
        for &(f, max_db) in &[
            (500.0, -1.0),
            (1000.0, -1.0),
            (2000.0, -1.0),
            (3000.0, -2.0),
        ] {
            let db = measure_db(f);
            assert!(
                db > max_db,
                "FIR at {f:.0} Hz attenuated to {db:.1} dB; expected > {max_db:.1} dB"
            );
        }
        // Stopband — should be ≥ 30 dB attenuated.
        for &(f, min_db) in &[(5000.0, -30.0), (6000.0, -40.0), (8000.0, -40.0)] {
            let db = measure_db(f);
            assert!(
                db < min_db,
                "FIR at {f:.0} Hz only attenuated to {db:.1} dB; expected < {min_db:.1} dB"
            );
        }
    }

    /// Round-tripping a 6500 Hz tone through the resampler (48 kHz →
    /// 8 kHz → 48 kHz) must NOT alias to 1500 Hz. Without the FIR
    /// anti-alias filter, linear resampling of a 6500 Hz tone at
    /// 48 kHz produces full-amplitude alias content at 8000 - 6500 =
    /// 1500 Hz on the way down. With the filter, the 6500 Hz content
    /// is attenuated below the audible noise floor before
    /// downsampling — the 1500 Hz alias is gone.
    #[test]
    fn resampler_does_not_alias_6500hz_tone() {
        // 0.1 s of 6500 Hz tone at 48 kHz, peak amplitude 0.5
        let n = 4800_usize;
        let fs_in = 48_000.0_f32;
        let mut input = Vec::with_capacity(n);
        for i in 0..n {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test: i bounded by n=4800, exact in f32."
            )]
            let t = i as f32 / fs_in;
            input.push(0.5 * (t * 2.0 * std::f32::consts::PI * 6500.0).sin());
        }

        // Downsample to 8 kHz
        let mut down = Vec::new();
        resample_linear(&input, 48_000, 8_000, &mut down);

        // Measure energy at 1500 Hz (the would-be alias frequency) in
        // the 8 kHz output. Should be near zero — the FIR pre-filter
        // already killed the 6500 Hz content.
        let mut re = 0.0_f32;
        let mut im = 0.0_f32;
        for (i, &s) in down.iter().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test: i bounded by down.len() (~800), exact in f32."
            )]
            let t = i as f32 / 8000.0;
            let phase = 2.0 * std::f32::consts::PI * 1500.0 * t;
            re += s * phase.cos();
            im += s * phase.sin();
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "test: down.len() small, exact in f32."
        )]
        let n_down = down.len() as f32;
        let alias_mag = re.hypot(im) / n_down;
        // Original 6500 Hz tone had amplitude 0.5; alias-free output
        // should have ≤ 0.005 (40 dB below the input).
        assert!(
            alias_mag < 0.05,
            "1500 Hz alias amplitude is {alias_mag:.4}, expected < 0.05 \
             (this means the anti-alias FIR is leaking high-frequency \
             content back into the speech band — sextant↔sextant garble \
             noise will return)."
        );
    }
}
